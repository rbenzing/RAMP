use crate::state::Service;
use std::time::Duration;

#[allow(dead_code)]
/// All system mutations MUST originate from one of these events.
/// Events are processed in FIFO order by the single-threaded reducer loop.
#[derive(Debug, Clone)]
pub enum Event {
    // User / IPC commands
    StartService(Service),
    StopService(Service),
    RestartService(Service),

    // OS / process signals
    ProcessExit {
        service: Service,
        exit_code: Option<i32>,
    },
    ProcessReady(Service),
    ProcessSpawnFailed {
        service: Service,
        reason: String,
    },

    // Health check results
    HealthCheckPass(Service),
    HealthCheckFail(Service),

    // Port management
    PortConflictDetected(Service),

    // Config
    ConfigReloaded,

    // Internal
    FatalError {
        service: Service,
        reason: String,
    },
    AutoRetry(Service),
    Tick,

    // Shutdown
    ShutdownAll,
}

/// Side effects produced by the reducer. Executed by the executor AFTER state mutation.
/// Side effects MUST never mutate state directly — they emit follow-up events.
#[derive(Debug)]
pub enum SideEffect {
    SpawnService(Service),
    KillService(Service),
    ScheduleRetry { service: Service, delay: Duration },
    StartReadinessCheck(Service),
    StopHealthCheck(Service),
    LogEvent(String),
    PersistDesiredState,
}
