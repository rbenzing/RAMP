mod config;
mod events;
mod executor;
mod health;
mod logger;
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
        eprintln!("FATAL: cannot write default config: {e}");
        std::process::exit(1);
    }

    // Load config — reject if invalid
    let config = match load_config(&install_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FATAL: invalid ramp.toml: {e}");
            std::process::exit(1);
        }
    };

    // Load persisted desired state (if any)
    let persisted = load_persisted_state(&install_dir);

    // Build initial AppState
    let mut app_state = AppState::new(config.clone());
    app_state.apache.desired = persisted.apache_desired;
    app_state.mysql.desired = persisted.mysql_desired;

    // Shared state for UI reads
    let shared_state = Arc::new(Mutex::new(app_state.clone()));
    let shared_state_writer = shared_state.clone();

    // Event channel (bounded — backpressure per spec)
    let (tx, rx) = crossbeam_channel::bounded::<Event>(256);

    // Tick timer thread
    let tick_tx = tx.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(state::HEALTH_CHECK_INTERVAL);
        if tick_tx.send(Event::Tick).is_err() {
            break;
        }
    });

    let log = SharedLog::new();
    let log_for_ui = log.clone();

    // Channel for tray → show window
    let (show_tx, show_rx) = crossbeam_channel::bounded::<()>(4);

    // System tray thread
    let tray_tx = tx.clone();
    let show_tx2 = show_tx.clone();
    std::thread::spawn(move || tray::run_tray(tray_tx, show_tx2));

    // Event loop thread
    let config_for_executor = config.clone();
    let log_for_loop = log.clone();
    let tx_for_loop = tx.clone();
    std::thread::spawn(move || {
        let mut state = app_state;
        let mut executor = Executor::new(config_for_executor, tx_for_loop.clone(), log_for_loop);

        // Debounce tracking: last command time per service
        let mut last_cmd: HashMap<String, Instant> = HashMap::new();

        // If desired state says services should be running, start them on boot
        for svc in [Service::Apache, Service::Mysql] {
            if state.service(svc).desired == DesiredServiceState::Running {
                let _ = tx_for_loop.send(Event::StartService(svc));
            }
        }

        while let Ok(event) = rx.recv() {
            // Debounce user commands
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

            // Capture pre-transition states for health check lifecycle
            let apache_was = state.apache.state;
            let mysql_was = state.mysql.state;

            let (new_state, effects) = reducer(state, event);
            state = new_state;

            // Start health checks when a service transitions to Running
            if apache_was != ServiceState::Running && state.apache.state == ServiceState::Running {
                executor.start_health_check(Service::Apache);
            }
            if mysql_was != ServiceState::Running && state.mysql.state == ServiceState::Running {
                executor.start_health_check(Service::Mysql);
            }

            executor.execute(effects, &state);

            // Update shared state for UI
            if let Ok(mut s) = shared_state_writer.lock() {
                *s = state.clone();
            }
        }
    });

    // Run egui on the main thread (required by Windows GUI)
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
