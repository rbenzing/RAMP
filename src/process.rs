use crate::state::{RampConfig, Service};
use crossbeam_channel::Sender;
use std::path::PathBuf;
use std::process::Child;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

use crate::events::Event;
use crate::paths::validate_critical_path;

/// RAII wrapper: dropping closes the Job Object handle, which (with KILL_ON_JOB_CLOSE)
/// terminates the entire process tree including all children.
pub struct JobHandle(pub HANDLE);

impl Drop for JobHandle {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

// SAFETY: Job handles are only sent across threads within this module under controlled ownership.
unsafe impl Send for JobHandle {}

/// A running service process. Fields are pub(crate) so the executor's watcher can access them.
pub struct ServiceProcess {
    pub job_handle: JobHandle,
    pub child: Child,
    pub service: Service,
}

impl ServiceProcess {
    /// Force-kill by closing the Job Object (kills entire process tree), then wait for cleanup.
    pub fn kill(self) {
        drop(self.job_handle); // KILL_ON_JOB_CLOSE terminates the tree
        let mut child = self.child;
        let _ = child.wait(); // reap
    }
}

/// Validate binary, spawn process, attach to Windows Job Object.
/// Returns Err if any step fails — caller MUST NOT start the service in that case.
pub fn spawn_service(
    svc: Service,
    cfg: &RampConfig,
    _tx: Sender<Event>,
) -> Result<ServiceProcess, String> {
    let (bin, args, work_dir) = service_params(svc, cfg);

    validate_critical_path(&bin, &cfg.install_dir, false)
        .map_err(|e| format!("binary validation: {e}"))?;

    if !bin.exists() {
        return Err(format!("binary not found: {}", bin.display()));
    }

    // Create Job Object
    let job_raw =
        unsafe { CreateJobObjectW(None, None) }.map_err(|e| format!("CreateJobObjectW: {e}"))?;

    // Configure: kill all processes when the job handle is closed
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    unsafe {
        SetInformationJobObject(
            job_raw,
            JobObjectExtendedLimitInformation,
            &raw const info as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(|e| {
            let _ = CloseHandle(job_raw);
            format!("SetInformationJobObject: {e}")
        })?;
    }

    // Spawn with sanitized environment, explicit working directory, absolute binary path
    let mut child = std::process::Command::new(&bin)
        .args(&args)
        .current_dir(&work_dir)
        .env_clear()
        .env(
            "SystemRoot",
            std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into()),
        )
        .env(
            "TEMP",
            std::env::var("TEMP").unwrap_or_else(|_| "C:\\Windows\\Temp".into()),
        )
        .spawn()
        .map_err(|e| {
            unsafe {
                let _ = CloseHandle(job_raw);
            }
            format!("spawn: {e}")
        })?;

    // Open the child process handle and assign it to the Job Object
    let pid = child.id();
    let proc_handle = unsafe { OpenProcess(PROCESS_ALL_ACCESS, false, pid) }.map_err(|e| {
        let _ = child.kill();
        unsafe {
            let _ = CloseHandle(job_raw);
        }
        format!("OpenProcess: {e}")
    })?;

    let assign = unsafe { AssignProcessToJobObject(job_raw, proc_handle) };
    unsafe {
        let _ = CloseHandle(proc_handle);
    }

    if let Err(e) = assign {
        let _ = child.kill();
        unsafe {
            let _ = CloseHandle(job_raw);
        }
        return Err(format!(
            "AssignProcessToJobObject: {e} — service must not start"
        ));
    }

    Ok(ServiceProcess {
        job_handle: JobHandle(job_raw),
        child,
        service: svc,
    })
}

/// Pre-check whether a port is available by attempting a bind.
pub fn check_port_available(port: u16) -> bool {
    use std::net::TcpListener;
    TcpListener::bind(format!("127.0.0.1:{port}")).is_ok()
}

fn service_params(svc: Service, cfg: &RampConfig) -> (PathBuf, Vec<String>, PathBuf) {
    match svc {
        Service::Apache => {
            let bin = cfg.apache.bin.clone();
            // ServerRoot is the apache\ dir; work_dir must be there so relative log paths resolve
            let work_dir = cfg.install_dir.join("apache");
            let args = vec![
                "-f".into(),
                cfg.apache.conf.display().to_string(),
                "-DFOREGROUND".into(),
            ];
            (bin, args, work_dir)
        }
        Service::Mysql => {
            let bin = cfg.mysql.bin.clone();
            let work_dir = cfg.install_dir.join("mysql");
            let args = vec![
                format!("--defaults-file={}", cfg.mysql.ini.display()),
                "--console".into(),
            ];
            (bin, args, work_dir)
        }
    }
}
