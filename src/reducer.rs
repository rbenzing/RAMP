use crate::events::{Event, SideEffect};
use crate::state::{
    retry_delay, AppState, DesiredServiceState, Service, ServiceState, MAX_RETRIES,
};

/// Pure reducer: STATE + EVENT → (NEW STATE, SIDE EFFECTS).
/// No I/O. No panics on invalid transitions — they are silently rejected with a log.
/// This function is the only place state may be mutated.
pub fn reducer(mut state: AppState, event: Event) -> (AppState, Vec<SideEffect>) {
    let mut effects = Vec::new();

    match event {
        // ── User commands ────────────────────────────────────────────────────
        Event::StartService(svc) => {
            let status = state.service(svc);
            match status.state {
                ServiceState::Stopped | ServiceState::Error => {
                    state.service_mut(svc).state = ServiceState::Starting;
                    state.service_mut(svc).desired = DesiredServiceState::Running;
                    state.service_mut(svc).retry_count = 0;
                    state.service_mut(svc).last_error = None;
                    effects.push(SideEffect::SpawnService(svc));
                    effects.push(SideEffect::StartReadinessCheck(svc));
                    effects.push(SideEffect::LogEvent(format!("{svc}: starting")));
                    effects.push(SideEffect::PersistDesiredState);
                }
                ServiceState::Crashed => {
                    // Treat same as Stopped — reset and try again
                    state.service_mut(svc).state = ServiceState::Starting;
                    state.service_mut(svc).desired = DesiredServiceState::Running;
                    state.service_mut(svc).retry_count = 0;
                    state.service_mut(svc).last_error = None;
                    effects.push(SideEffect::SpawnService(svc));
                    effects.push(SideEffect::StartReadinessCheck(svc));
                    effects.push(SideEffect::LogEvent(format!(
                        "{svc}: restarting after crash"
                    )));
                    effects.push(SideEffect::PersistDesiredState);
                }
                other => {
                    effects.push(SideEffect::LogEvent(format!(
                        "{svc}: StartService ignored in state {other}"
                    )));
                }
            }
        }

        Event::StopService(svc) => {
            let status = state.service(svc);
            match status.state {
                ServiceState::Running | ServiceState::Starting => {
                    state.service_mut(svc).state = ServiceState::Stopping;
                    state.service_mut(svc).desired = DesiredServiceState::Stopped;
                    effects.push(SideEffect::StopHealthCheck(svc));
                    effects.push(SideEffect::KillService(svc));
                    effects.push(SideEffect::LogEvent(format!("{svc}: stopping")));
                    effects.push(SideEffect::PersistDesiredState);
                }
                ServiceState::Crashed | ServiceState::Error => {
                    // Already not running — just update desired
                    state.service_mut(svc).state = ServiceState::Stopped;
                    state.service_mut(svc).desired = DesiredServiceState::Stopped;
                    effects.push(SideEffect::PersistDesiredState);
                }
                other => {
                    effects.push(SideEffect::LogEvent(format!(
                        "{svc}: StopService ignored in state {other}"
                    )));
                }
            }
        }

        Event::RestartService(svc) => {
            // Decompose into stop then a queued start via AutoRetry with 0 delay logic.
            // Simpler: emit Stop + Start as two events via effects is not possible (effects
            // can't emit events directly). Instead transition through Stopping and rely on
            // ProcessExit to re-check desired_state.
            let status = state.service(svc);
            match status.state {
                ServiceState::Running | ServiceState::Starting => {
                    state.service_mut(svc).state = ServiceState::Stopping;
                    state.service_mut(svc).desired = DesiredServiceState::Running; // keep desired Running
                    effects.push(SideEffect::StopHealthCheck(svc));
                    effects.push(SideEffect::KillService(svc));
                    effects.push(SideEffect::LogEvent(format!("{svc}: restarting")));
                }
                ServiceState::Stopped | ServiceState::Crashed | ServiceState::Error => {
                    state.service_mut(svc).state = ServiceState::Starting;
                    state.service_mut(svc).desired = DesiredServiceState::Running;
                    state.service_mut(svc).retry_count = 0;
                    state.service_mut(svc).last_error = None;
                    effects.push(SideEffect::SpawnService(svc));
                    effects.push(SideEffect::StartReadinessCheck(svc));
                    effects.push(SideEffect::LogEvent(format!("{svc}: starting (restart)")));
                }
                ServiceState::Stopping => {
                    // Already stopping; desired=Running means ProcessExit handler will restart.
                    state.service_mut(svc).desired = DesiredServiceState::Running;
                    effects.push(SideEffect::LogEvent(format!(
                        "{svc}: will restart once stopped"
                    )));
                }
            }
        }

        // ── Process lifecycle ────────────────────────────────────────────────
        Event::ProcessReady(svc) => {
            if state.service(svc).state == ServiceState::Starting {
                state.service_mut(svc).state = ServiceState::Running;
                state.service_mut(svc).retry_count = 0;
                state.service_mut(svc).health_fail_streak = 0;
                effects.push(SideEffect::LogEvent(format!("{svc}: ready")));
            } else {
                effects.push(SideEffect::LogEvent(format!(
                    "{svc}: ProcessReady ignored in state {}",
                    state.service(svc).state
                )));
            }
        }

        Event::ProcessExit {
            service: svc,
            exit_code,
        } => {
            match state.service(svc).state {
                ServiceState::Stopping => {
                    state.service_mut(svc).state = ServiceState::Stopped;
                    effects.push(SideEffect::LogEvent(format!(
                        "{svc}: stopped (exit {exit_code:?})"
                    )));
                    // If desired is Running (e.g. after RestartService), auto-start
                    if state.service(svc).desired == DesiredServiceState::Running {
                        state.service_mut(svc).state = ServiceState::Starting;
                        state.service_mut(svc).retry_count = 0;
                        effects.push(SideEffect::SpawnService(svc));
                        effects.push(SideEffect::StartReadinessCheck(svc));
                        effects.push(SideEffect::LogEvent(format!(
                            "{svc}: restarting per desired state"
                        )));
                    }
                }
                ServiceState::Starting | ServiceState::Running => {
                    // Unexpected exit → Crashed
                    state.service_mut(svc).state = ServiceState::Crashed;
                    state.service_mut(svc).last_error =
                        Some(format!("exited unexpectedly (code {exit_code:?})"));
                    effects.push(SideEffect::StopHealthCheck(svc));
                    effects.push(SideEffect::LogEvent(format!(
                        "{svc}: crashed (exit {exit_code:?})"
                    )));
                    // Auto-retry if desired is Running and retries remain
                    if state.service(svc).desired == DesiredServiceState::Running {
                        let retry = state.service(svc).retry_count;
                        if let Some(delay) = retry_delay(retry) {
                            state.service_mut(svc).retry_count += 1;
                            effects.push(SideEffect::ScheduleRetry {
                                service: svc,
                                delay,
                            });
                            effects.push(SideEffect::LogEvent(format!(
                                "{svc}: retry {} of {} in {:?}",
                                retry + 1,
                                MAX_RETRIES,
                                delay
                            )));
                        } else {
                            state.service_mut(svc).state = ServiceState::Error;
                            state.service_mut(svc).last_error =
                                Some("max retries exceeded".to_string());
                            effects.push(SideEffect::LogEvent(format!(
                                "{svc}: max retries exceeded → Error"
                            )));
                        }
                    }
                }
                other => {
                    effects.push(SideEffect::LogEvent(format!(
                        "{svc}: ProcessExit ignored in state {other}"
                    )));
                }
            }
        }

        Event::ProcessSpawnFailed {
            service: svc,
            reason,
        } => {
            state.service_mut(svc).state = ServiceState::Error;
            state.service_mut(svc).last_error = Some(reason.clone());
            effects.push(SideEffect::LogEvent(format!(
                "{svc}: spawn failed — {reason}"
            )));
        }

        // ── Health checks ────────────────────────────────────────────────────
        Event::HealthCheckPass(svc) => {
            if state.service(svc).state == ServiceState::Running {
                state.service_mut(svc).health_fail_streak = 0;
            }
        }

        Event::HealthCheckFail(svc) => {
            if state.service(svc).state == ServiceState::Running {
                let streak = state.service(svc).health_fail_streak + 1;
                state.service_mut(svc).health_fail_streak = streak;
                effects.push(SideEffect::LogEvent(format!(
                    "{svc}: health check failed ({streak} consecutive)"
                )));
                if streak >= crate::state::HEALTH_FAIL_THRESHOLD {
                    // Treat as unexpected exit for retry logic
                    state.service_mut(svc).state = ServiceState::Crashed;
                    state.service_mut(svc).last_error =
                        Some(format!("{streak} consecutive health check failures"));
                    effects.push(SideEffect::StopHealthCheck(svc));
                    effects.push(SideEffect::KillService(svc));
                    if state.service(svc).desired == DesiredServiceState::Running {
                        let retry = state.service(svc).retry_count;
                        if let Some(delay) = retry_delay(retry) {
                            state.service_mut(svc).retry_count += 1;
                            effects.push(SideEffect::ScheduleRetry {
                                service: svc,
                                delay,
                            });
                        } else {
                            state.service_mut(svc).state = ServiceState::Error;
                            state.service_mut(svc).last_error =
                                Some("max retries exceeded after health failures".to_string());
                            effects.push(SideEffect::LogEvent(format!(
                                "{svc}: max retries exceeded → Error"
                            )));
                        }
                    }
                }
            }
        }

        // ── Port conflict ────────────────────────────────────────────────────
        Event::PortConflictDetected(svc) => {
            state.service_mut(svc).state = ServiceState::Error;
            state.service_mut(svc).last_error = Some("port already in use".to_string());
            effects.push(SideEffect::LogEvent(format!(
                "{svc}: port conflict detected → Error"
            )));
        }

        // ── Auto-retry (from executor timer) ────────────────────────────────
        Event::AutoRetry(svc) => {
            if state.service(svc).state == ServiceState::Crashed
                && state.service(svc).desired == DesiredServiceState::Running
            {
                state.service_mut(svc).state = ServiceState::Starting;
                effects.push(SideEffect::SpawnService(svc));
                effects.push(SideEffect::StartReadinessCheck(svc));
                effects.push(SideEffect::LogEvent(format!("{svc}: auto-retry starting")));
            }
        }

        // ── Fatal error ──────────────────────────────────────────────────────
        Event::FatalError {
            service: svc,
            reason,
        } => {
            state.service_mut(svc).state = ServiceState::Error;
            state.service_mut(svc).last_error = Some(reason.clone());
            effects.push(SideEffect::StopHealthCheck(svc));
            effects.push(SideEffect::LogEvent(format!("{svc}: FATAL — {reason}")));
        }

        // ── Config reload ────────────────────────────────────────────────────
        Event::ConfigReloaded => {
            effects.push(SideEffect::LogEvent("config reloaded".to_string()));
        }

        // ── Tick (drives health check cycle — executor owns the timer) ───────
        Event::Tick => {
            // Tick is consumed by the executor to fire health checks.
            // The reducer records nothing for Tick.
        }

        // ── Shutdown all ─────────────────────────────────────────────────────
        Event::ShutdownAll => {
            for svc in [Service::Apache, Service::Mysql, Service::Php] {
                match state.service(svc).state {
                    ServiceState::Running | ServiceState::Starting => {
                        state.service_mut(svc).state = ServiceState::Stopping;
                        state.service_mut(svc).desired = DesiredServiceState::Stopped;
                        effects.push(SideEffect::StopHealthCheck(svc));
                        effects.push(SideEffect::KillService(svc));
                    }
                    _ => {
                        state.service_mut(svc).desired = DesiredServiceState::Stopped;
                    }
                }
            }
            effects.push(SideEffect::LogEvent(
                "shutting down all services".to_string(),
            ));
            effects.push(SideEffect::PersistDesiredState);
        }
    }

    (state, effects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event;
    use crate::state::{
        ApacheConfig, AppState, DesiredServiceState, MysqlConfig, PhpConfig, RampConfig, Service,
        ServiceState,
    };

    fn make_state() -> AppState {
        let config = RampConfig {
            install_dir: std::path::PathBuf::from("C:\\ramp"),
            apache: ApacheConfig {
                port: 80,
                bin: std::path::PathBuf::from("C:\\ramp\\apache\\bin\\httpd.exe"),
                conf: std::path::PathBuf::from("C:\\ramp\\apache\\conf\\httpd.conf"),
            },
            mysql: MysqlConfig {
                port: 3306,
                bin: std::path::PathBuf::from("C:\\ramp\\mysql\\bin\\mysqld.exe"),
                data_dir: std::path::PathBuf::from("C:\\ramp\\mysql\\data"),
                ini: std::path::PathBuf::from("C:\\ramp\\mysql\\my.ini"),
            },
            php: PhpConfig {
                port: 9000,
                bin: std::path::PathBuf::from("C:\\ramp\\php\\php-cgi.exe"),
                ini: std::path::PathBuf::from("C:\\ramp\\php\\php.ini"),
            },
        };
        AppState::new(config)
    }

    fn set_state(state: &mut AppState, svc: Service, s: ServiceState) {
        state.service_mut(svc).state = s;
    }

    // ── Valid transitions ──────────────────────────────────────────────────

    #[test]
    fn stopped_start_transitions_to_starting() {
        let state = make_state();
        let (new_state, effects) = reducer(state, Event::StartService(Service::Apache));
        assert_eq!(new_state.apache.state, ServiceState::Starting);
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::SpawnService(Service::Apache))));
    }

    #[test]
    fn starting_process_ready_transitions_to_running() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Starting);
        let (new_state, _) = reducer(state, Event::ProcessReady(Service::Apache));
        assert_eq!(new_state.apache.state, ServiceState::Running);
    }

    #[test]
    fn running_stop_transitions_to_stopping() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        let (new_state, effects) = reducer(state, Event::StopService(Service::Apache));
        assert_eq!(new_state.apache.state, ServiceState::Stopping);
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::KillService(Service::Apache))));
    }

    #[test]
    fn stopping_process_exit_transitions_to_stopped() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Stopping);
        let (new_state, _) = reducer(
            state,
            Event::ProcessExit {
                service: Service::Apache,
                exit_code: Some(0),
            },
        );
        assert_eq!(new_state.apache.state, ServiceState::Stopped);
    }

    #[test]
    fn running_process_exit_transitions_to_crashed() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        let (new_state, _) = reducer(
            state,
            Event::ProcessExit {
                service: Service::Apache,
                exit_code: Some(1),
            },
        );
        assert_eq!(new_state.apache.state, ServiceState::Crashed);
    }

    #[test]
    fn crashed_auto_retry_transitions_to_starting_when_desired_running() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Crashed);
        state.apache.desired = DesiredServiceState::Running;
        let (new_state, effects) = reducer(state, Event::AutoRetry(Service::Apache));
        assert_eq!(new_state.apache.state, ServiceState::Starting);
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::SpawnService(Service::Apache))));
    }

    #[test]
    fn fatal_error_any_state_transitions_to_error() {
        for initial in [
            ServiceState::Stopped,
            ServiceState::Starting,
            ServiceState::Running,
            ServiceState::Stopping,
            ServiceState::Crashed,
        ] {
            let mut state = make_state();
            set_state(&mut state, Service::Apache, initial);
            let (new_state, _) = reducer(
                state,
                Event::FatalError {
                    service: Service::Apache,
                    reason: "test".into(),
                },
            );
            assert_eq!(
                new_state.apache.state,
                ServiceState::Error,
                "FatalError from {initial} should → Error"
            );
        }
    }

    // ── Invalid transitions (must not mutate state) ───────────────────────

    #[test]
    fn start_ignored_when_already_starting() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Starting);
        let (new_state, effects) = reducer(state, Event::StartService(Service::Apache));
        assert_eq!(new_state.apache.state, ServiceState::Starting);
        // No SpawnService should be emitted
        assert!(!effects
            .iter()
            .any(|e| matches!(e, SideEffect::SpawnService(Service::Apache))));
    }

    #[test]
    fn start_ignored_when_running() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        let (new_state, _) = reducer(state, Event::StartService(Service::Apache));
        assert_eq!(new_state.apache.state, ServiceState::Running);
    }

    #[test]
    fn stop_ignored_when_already_stopped() {
        let state = make_state();
        let (new_state, _) = reducer(state, Event::StopService(Service::Apache));
        // Stopped stays Stopped (no KillService emitted)
        assert_eq!(new_state.apache.state, ServiceState::Stopped);
    }

    #[test]
    fn process_ready_ignored_when_not_starting() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        let (new_state, _) = reducer(state, Event::ProcessReady(Service::Apache));
        assert_eq!(new_state.apache.state, ServiceState::Running);
    }

    // ── Retry logic ───────────────────────────────────────────────────────

    #[test]
    fn crash_schedules_retry_when_desired_running() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        state.apache.desired = DesiredServiceState::Running;
        let (new_state, effects) = reducer(
            state,
            Event::ProcessExit {
                service: Service::Apache,
                exit_code: Some(1),
            },
        );
        assert_eq!(new_state.apache.state, ServiceState::Crashed);
        assert!(effects.iter().any(|e| matches!(
            e,
            SideEffect::ScheduleRetry {
                service: Service::Apache,
                ..
            }
        )));
    }

    #[test]
    fn crash_no_retry_when_desired_stopped() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        state.apache.desired = DesiredServiceState::Stopped;
        let (new_state, effects) = reducer(
            state,
            Event::ProcessExit {
                service: Service::Apache,
                exit_code: Some(1),
            },
        );
        assert_eq!(new_state.apache.state, ServiceState::Crashed);
        assert!(!effects
            .iter()
            .any(|e| matches!(e, SideEffect::ScheduleRetry { .. })));
    }

    #[test]
    fn max_retries_exceeded_transitions_to_error() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        state.apache.desired = DesiredServiceState::Running;
        state.apache.retry_count = MAX_RETRIES; // all retries exhausted

        let (new_state, effects) = reducer(
            state,
            Event::ProcessExit {
                service: Service::Apache,
                exit_code: Some(1),
            },
        );
        assert_eq!(new_state.apache.state, ServiceState::Error);
        assert!(!effects
            .iter()
            .any(|e| matches!(e, SideEffect::ScheduleRetry { .. })));
    }

    // ── Invariants ────────────────────────────────────────────────────────

    #[test]
    fn each_service_state_is_independent() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        let (new_state, _) = reducer(state, Event::StartService(Service::Mysql));
        assert_eq!(new_state.apache.state, ServiceState::Running);
        assert_eq!(new_state.mysql.state, ServiceState::Starting);
    }

    #[test]
    fn restart_from_running_sets_desired_running_and_kills() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        let (new_state, effects) = reducer(state, Event::RestartService(Service::Apache));
        assert_eq!(new_state.apache.state, ServiceState::Stopping);
        assert_eq!(new_state.apache.desired, DesiredServiceState::Running);
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::KillService(Service::Apache))));
    }

    #[test]
    fn shutdown_all_stops_running_services() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        set_state(&mut state, Service::Mysql, ServiceState::Running);
        set_state(&mut state, Service::Php, ServiceState::Running);
        let (new_state, effects) = reducer(state, Event::ShutdownAll);
        assert_eq!(new_state.apache.state, ServiceState::Stopping);
        assert_eq!(new_state.mysql.state, ServiceState::Stopping);
        assert_eq!(new_state.php.state, ServiceState::Stopping);
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::KillService(Service::Apache))));
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::KillService(Service::Mysql))));
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::KillService(Service::Php))));
    }

    #[test]
    fn php_service_state_machine_works() {
        // PHP follows the same state machine as Apache/MySQL
        let state = make_state();
        let (new_state, effects) = reducer(state, Event::StartService(Service::Php));
        assert_eq!(new_state.php.state, ServiceState::Starting);
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::SpawnService(Service::Php))));
    }

    #[test]
    fn health_fail_streak_accumulates_and_triggers_crash_at_threshold() {
        let mut state = make_state();
        set_state(&mut state, Service::Apache, ServiceState::Running);
        state.apache.desired = DesiredServiceState::Running;

        // Two failures — still Running
        for _ in 0..2 {
            let (s, _) = reducer(state.clone(), Event::HealthCheckFail(Service::Apache));
            state = s;
        }
        assert_eq!(state.apache.state, ServiceState::Running);

        // Third failure — crosses threshold
        let (new_state, effects) = reducer(state, Event::HealthCheckFail(Service::Apache));
        assert_eq!(new_state.apache.state, ServiceState::Crashed);
        assert!(effects
            .iter()
            .any(|e| matches!(e, SideEffect::KillService(Service::Apache))));
    }

    #[test]
    fn spawn_failed_transitions_to_error() {
        let state = make_state();
        let (new_state, _) = reducer(
            state,
            Event::ProcessSpawnFailed {
                service: Service::Apache,
                reason: "binary not found".into(),
            },
        );
        assert_eq!(new_state.apache.state, ServiceState::Error);
    }
}
