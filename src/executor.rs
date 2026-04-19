use crate::config::atomic_write;
use crate::events::{Event, SideEffect};
use crate::health::{poll_until_ready, run_health_checker};
use crate::logger::SharedLog;
use crate::process::{check_port_available, spawn_service, ServiceProcess};
use crate::state::{AppState, PersistedState, RampConfig, Service};
use crossbeam_channel::Sender;
use std::collections::HashMap;

/// Per-service runtime handles.
struct ServiceHandles {
    /// Channel to signal the watcher thread to force-kill the process.
    kill_tx: crossbeam_channel::Sender<()>,
    /// Channel to stop the health checker.
    health_stop_tx: Option<crossbeam_channel::Sender<()>>,
}

/// Executor translates SideEffects into real I/O. Owns all live process/thread handles.
pub struct Executor {
    config: RampConfig,
    tx: Sender<Event>,
    log: SharedLog,
    handles: HashMap<Service, ServiceHandles>,
}

impl Executor {
    pub fn new(config: RampConfig, tx: Sender<Event>, log: SharedLog) -> Self {
        Self {
            config,
            tx,
            log,
            handles: HashMap::new(),
        }
    }

    pub fn execute(&mut self, effects: Vec<SideEffect>, state: &AppState) {
        for effect in effects {
            match effect {
                SideEffect::SpawnService(svc) => self.do_spawn(svc),
                SideEffect::KillService(svc) => self.do_kill(svc),
                SideEffect::StartReadinessCheck(svc) => self.do_readiness_check(svc),
                SideEffect::StopHealthCheck(svc) => self.do_stop_health(svc),
                SideEffect::ScheduleRetry { service, delay } => {
                    self.do_schedule_retry(service, delay)
                }
                SideEffect::LogEvent(msg) => {
                    log::info!("{msg}");
                    self.log.push(msg);
                }
                SideEffect::PersistDesiredState => self.do_persist(state),
            }
        }
    }

    /// Start health checks for a service that just became Running.
    pub fn start_health_check(&mut self, svc: Service) {
        self.do_stop_health(svc);
        let port = self.port(svc);
        let (stop_tx, stop_rx) = crossbeam_channel::bounded(1);
        let entry = self.handles.entry(svc).or_insert_with(|| {
            let (kill_tx, _) = crossbeam_channel::bounded(1);
            ServiceHandles {
                kill_tx,
                health_stop_tx: None,
            }
        });
        entry.health_stop_tx = Some(stop_tx);
        let tx = self.tx.clone();
        std::thread::spawn(move || run_health_checker(svc, port, tx, stop_rx));
    }

    fn do_spawn(&mut self, svc: Service) {
        let port = self.port(svc);

        // Pre-check port
        if !check_port_available(port) {
            let _ = self.tx.send(Event::PortConflictDetected(svc));
            return;
        }

        // Kill any existing handles for this service
        self.do_kill(svc);

        let (kill_tx, kill_rx) = crossbeam_channel::bounded::<()>(1);

        match spawn_service(svc, &self.config, self.tx.clone()) {
            Ok(proc) => {
                self.handles.insert(
                    svc,
                    ServiceHandles {
                        kill_tx,
                        health_stop_tx: None,
                    },
                );
                let tx = self.tx.clone();
                std::thread::spawn(move || watcher(proc, tx, kill_rx));
            }
            Err(reason) => {
                let _ = self.tx.send(Event::ProcessSpawnFailed {
                    service: svc,
                    reason,
                });
            }
        }
    }

    fn do_kill(&mut self, svc: Service) {
        self.do_stop_health(svc);
        if let Some(h) = self.handles.remove(&svc) {
            // Signal watcher thread to kill the process
            let _ = h.kill_tx.send(());
        }
    }

    fn do_readiness_check(&self, svc: Service) {
        let port = self.port(svc);
        let tx = self.tx.clone();
        std::thread::spawn(move || poll_until_ready(svc, port, tx));
    }

    fn do_stop_health(&mut self, svc: Service) {
        if let Some(h) = self.handles.get_mut(&svc) {
            if let Some(stop) = h.health_stop_tx.take() {
                let _ = stop.send(());
            }
        }
    }

    fn do_schedule_retry(&self, svc: Service, delay: std::time::Duration) {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let _ = tx.send(Event::AutoRetry(svc));
        });
    }

    fn do_persist(&self, state: &AppState) {
        let persisted = PersistedState {
            apache_desired: state.apache.desired,
            mysql_desired: state.mysql.desired,
        };
        let path = self.config.install_dir.join("ramp.state");
        match serde_json::to_vec_pretty(&persisted) {
            Ok(data) => {
                if let Err(e) = atomic_write(&path, &data) {
                    log::warn!("persist state failed: {e}");
                }
            }
            Err(e) => log::warn!("serialize state failed: {e}"),
        }
    }

    fn port(&self, svc: Service) -> u16 {
        match svc {
            Service::Apache => self.config.apache.port,
            Service::Mysql => self.config.mysql.port,
        }
    }
}

/// Watches a running process. Kills it if a kill signal arrives, or emits ProcessExit naturally.
fn watcher(mut proc: ServiceProcess, tx: Sender<Event>, kill_rx: crossbeam_channel::Receiver<()>) {
    let svc = proc.service;

    // Poll the child with try_wait so we can also check for kill signals.
    loop {
        // Check if kill was requested
        if kill_rx.try_recv().is_ok() {
            proc.kill();
            // ProcessExit will be emitted naturally after kill via the process exit path,
            // but since we consumed the child in kill(), emit it explicitly.
            let _ = tx.send(Event::ProcessExit {
                service: svc,
                exit_code: None,
            });
            return;
        }

        // Non-blocking wait on child
        match proc.child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code();
                drop(proc.job_handle);
                let _ = tx.send(Event::ProcessExit {
                    service: svc,
                    exit_code: code,
                });
                return;
            }
            Ok(None) => {
                // Still running — sleep briefly and loop
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                log::warn!("{svc}: try_wait error: {e}");
                let _ = tx.send(Event::ProcessExit {
                    service: svc,
                    exit_code: None,
                });
                return;
            }
        }
    }
}
