use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    Crashed,
    Error,
}

impl std::fmt::Display for ServiceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceState::Stopped => write!(f, "Stopped"),
            ServiceState::Starting => write!(f, "Starting"),
            ServiceState::Running => write!(f, "Running"),
            ServiceState::Stopping => write!(f, "Stopping"),
            ServiceState::Crashed => write!(f, "Crashed"),
            ServiceState::Error => write!(f, "Error"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DesiredServiceState {
    Running,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Service {
    Apache,
    Mysql,
}

impl std::fmt::Display for Service {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Service::Apache => write!(f, "Apache"),
            Service::Mysql => write!(f, "MySQL"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServiceStatus {
    pub state: ServiceState,
    pub desired: DesiredServiceState,
    pub retry_count: u32,
    pub last_error: Option<String>,
    pub health_fail_streak: u32,
}

impl ServiceStatus {
    pub fn new() -> Self {
        Self {
            state: ServiceState::Stopped,
            desired: DesiredServiceState::Stopped,
            retry_count: 0,
            last_error: None,
            health_fail_streak: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApacheConfig {
    pub port: u16,
    pub bin: PathBuf,
    pub conf: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MysqlConfig {
    pub port: u16,
    pub bin: PathBuf,
    pub data_dir: PathBuf,
    pub ini: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RampConfig {
    pub install_dir: PathBuf,
    pub apache: ApacheConfig,
    pub mysql: MysqlConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortState {
    pub apache_bound: bool,
    pub mysql_bound: bool,
}

impl PortState {
    pub fn new() -> Self {
        Self {
            apache_bound: false,
            mysql_bound: false,
        }
    }
}

/// The complete application state. Owned exclusively by the reducer.
/// Never mutated outside of reducer(state, event) → (state, effects).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AppState {
    pub apache: ServiceStatus,
    pub mysql: ServiceStatus,
    pub config: RampConfig,
    pub ports: PortState,
}

impl AppState {
    pub fn new(config: RampConfig) -> Self {
        Self {
            apache: ServiceStatus::new(),
            mysql: ServiceStatus::new(),
            config,
            ports: PortState::new(),
        }
    }

    pub fn service(&self, svc: Service) -> &ServiceStatus {
        match svc {
            Service::Apache => &self.apache,
            Service::Mysql => &self.mysql,
        }
    }

    pub fn service_mut(&mut self, svc: Service) -> &mut ServiceStatus {
        match svc {
            Service::Apache => &mut self.apache,
            Service::Mysql => &mut self.mysql,
        }
    }
}

/// Persisted across restarts — records what the user wants running.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    pub apache_desired: DesiredServiceState,
    pub mysql_desired: DesiredServiceState,
}

impl PersistedState {
    pub fn default_stopped() -> Self {
        Self {
            apache_desired: DesiredServiceState::Stopped,
            mysql_desired: DesiredServiceState::Stopped,
        }
    }
}

/// Retry backoff schedule per spec: 1s → 2s → 4s → 8s → STOP (max 4 retries).
pub const MAX_RETRIES: u32 = 4;
pub const RETRY_DELAYS: [u64; 4] = [1, 2, 4, 8];

pub fn retry_delay(retry_count: u32) -> Option<Duration> {
    let idx = retry_count as usize;
    RETRY_DELAYS.get(idx).map(|&s| Duration::from_secs(s))
}

pub const APACHE_READY_TIMEOUT: Duration = Duration::from_secs(3);
pub const MYSQL_READY_TIMEOUT: Duration = Duration::from_secs(5);
pub const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(2);
pub const HEALTH_FAIL_THRESHOLD: u32 = 3;
#[allow(dead_code)]
pub const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(5);
pub const COMMAND_DEBOUNCE: Duration = Duration::from_millis(500);
