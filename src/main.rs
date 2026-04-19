mod apache_conf;
mod config;
mod events;
mod executor;
mod health;
mod logger;
mod mysql_conf;
mod paths;
mod process;
mod reducer;
mod state;
mod tray;
mod ui;

use config::{load_config, write_default_config};
use events::Event;
use executor::Executor;
use logger::SharedLog;
use reducer::reducer;
use state::{
    AppState, DesiredServiceState, PersistedState, Service, ServiceState, COMMAND_DEBOUNCE,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    // Resolve install_dir from the executable's location
    let install_dir = std::env::current_exe()
        .expect("cannot resolve executable path")
        .parent()
        .expect("executable has no parent directory")
        .to_path_buf();

    log::info!("RAMP starting — install_dir: {}", install_dir.display());

    // Ensure ramp.toml exists
    if let Err(e) = write_default_config(&install_dir) {
        fatal(&format!("cannot write default config: {e}"));
    }

    // Load and validate config
    let config = match load_config(&install_dir) {
        Ok(c) => c,
        Err(e) => fatal(&format!("invalid ramp.toml: {e}")),
    };

    // --- Startup provisioning (idempotent, safe to run every launch) ------

    // 1. Create required runtime directories
    if let Err(e) = create_runtime_dirs(&config) {
        fatal(&format!("cannot create runtime directories: {e}"));
    }

    // 2. Generate httpd.conf if missing
    if let Err(e) = apache_conf::ensure_httpd_conf(&config) {
        fatal(&format!("cannot generate httpd.conf: {e}"));
    }
    if let Err(e) = apache_conf::ensure_htdocs(&config) {
        fatal(&format!("cannot create htdocs: {e}"));
    }

    // 3. Generate my.ini if missing
    if let Err(e) = mysql_conf::ensure_my_ini(&config) {
        fatal(&format!("cannot generate my.ini: {e}"));
    }

    // 4. Initialize MySQL data directory on first run (blocking)
    if mysql_conf::needs_initialization(&config) {
        log::info!("MySQL data directory is empty — running --initialize-insecure");
        if let Err(e) = mysql_conf::initialize_mysql(&config) {
            fatal(&format!("MySQL initialization failed: {e}"));
        }
    }

    // --- Event loop setup -------------------------------------------------

    let persisted = load_persisted_state(&install_dir);

    let mut app_state = AppState::new(config.clone());
    app_state.apache.desired = persisted.apache_desired;
    app_state.mysql.desired = persisted.mysql_desired;

    let shared_state = Arc::new(Mutex::new(app_state.clone()));
    let shared_state_writer = shared_state.clone();

    // Bounded event channel — backpressure per spec
    let (tx, rx) = crossbeam_channel::bounded::<Event>(256);

    // Tick timer thread (drives health check cycle)
    let tick_tx = tx.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(state::HEALTH_CHECK_INTERVAL);
        if tick_tx.send(Event::Tick).is_err() {
            break;
        }
    });

    let log = SharedLog::new();
    let log_for_ui = log.clone();

    // Channel: tray → show egui window
    let (show_tx, show_rx) = crossbeam_channel::bounded::<()>(4);

    // System tray thread
    let tray_tx = tx.clone();
    std::thread::spawn(move || tray::run_tray(tray_tx, show_tx));

    // Event loop thread
    let config_for_executor = config.clone();
    let log_for_loop = log.clone();
    let tx_for_loop = tx.clone();
    std::thread::spawn(move || {
        let mut state = app_state;
        let mut executor = Executor::new(config_for_executor, tx_for_loop.clone(), log_for_loop);
        let mut last_cmd: HashMap<String, Instant> = HashMap::new();

        // Restore desired running services on startup
        for svc in [Service::Apache, Service::Mysql] {
            if state.service(svc).desired == DesiredServiceState::Running {
                let _ = tx_for_loop.send(Event::StartService(svc));
            }
        }

        while let Ok(event) = rx.recv() {
            // Debounce rapid user commands
            if let Some(key) = debounce_key(&event) {
                let now = Instant::now();
                if let Some(&last) = last_cmd.get(&key) {
                    if now.duration_since(last) < COMMAND_DEBOUNCE {
                        log::debug!("debounced: {key}");
                        continue;
                    }
                }
                last_cmd.insert(key, now);
            }

            let apache_was = state.apache.state;
            let mysql_was = state.mysql.state;

            let (new_state, effects) = reducer(state, event);
            state = new_state;

            // Start health checks when a service first reaches Running
            if apache_was != ServiceState::Running && state.apache.state == ServiceState::Running {
                executor.start_health_check(Service::Apache);
            }
            if mysql_was != ServiceState::Running && state.mysql.state == ServiceState::Running {
                executor.start_health_check(Service::Mysql);
            }

            executor.execute(effects, &state);

            if let Ok(mut s) = shared_state_writer.lock() {
                *s = state.clone();
            }
        }
    });

    // egui must run on the main thread (Windows GUI requirement)
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RAMP")
            .with_inner_size([520.0, 480.0])
            .with_min_inner_size([400.0, 300.0]),
        ..Default::default()
    };

    eframe::run_native(
        "RAMP",
        native_options,
        Box::new(|_cc| {
            Box::new(ui::RampApp::new(shared_state, tx, log_for_ui, show_rx))
                as Box<dyn eframe::App>
        }),
    )
    .unwrap_or_else(|e| {
        eprintln!("GUI error: {e}");
        std::process::exit(1);
    });
}

/// Create all runtime directories that must exist before services start.
fn create_runtime_dirs(cfg: &crate::state::RampConfig) -> Result<(), String> {
    let dirs = [
        cfg.install_dir.join("logs"),
        cfg.install_dir.join("apache").join("logs"),
        cfg.install_dir.join("apache").join("conf"),
        cfg.install_dir.join("mysql").join("logs"),
        cfg.mysql.data_dir.clone(),
    ];
    for dir in &dirs {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    Ok(())
}

fn load_persisted_state(install_dir: &std::path::Path) -> PersistedState {
    let path = install_dir.join("ramp.state");
    std::fs::read(&path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_else(PersistedState::default_stopped)
}

fn debounce_key(event: &Event) -> Option<String> {
    match event {
        Event::StartService(s) => Some(format!("start:{s}")),
        Event::StopService(s) => Some(format!("stop:{s}")),
        Event::RestartService(s) => Some(format!("restart:{s}")),
        _ => None,
    }
}

/// Print a fatal error and exit. Never returns.
fn fatal(msg: &str) -> ! {
    eprintln!("FATAL: {msg}");
    log::error!("{msg}");
    std::process::exit(1);
}
