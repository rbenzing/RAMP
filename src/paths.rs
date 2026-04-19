use std::path::{Path, PathBuf};

/// Validates and resolves all install-relative paths.
/// Enforces: absolute paths only, no symlinks for critical paths, no traversal.
#[allow(dead_code)]
pub struct InstallPaths {
    pub root: PathBuf,
    pub config: PathBuf,
    pub state_file: PathBuf,
    pub log_file: PathBuf,
    pub apache_bin: PathBuf,
    pub apache_conf: PathBuf,
    pub apache_logs: PathBuf,
    pub mysql_bin: PathBuf,
    pub mysql_data: PathBuf,
    pub mysql_ini: PathBuf,
    pub php_bin: PathBuf,
    pub php_ini: PathBuf,
    pub php_logs: PathBuf,
}

impl InstallPaths {
    pub fn from_install_dir(install_dir: &Path) -> Result<Self, String> {
        let root = install_dir.to_path_buf();
        if !root.is_absolute() {
            return Err(format!(
                "install_dir must be absolute, got: {}",
                root.display()
            ));
        }
        Ok(Self {
            config: root.join("ramp.toml"),
            state_file: root.join("ramp.state"),
            log_file: root.join("logs").join("ramp.log"),
            apache_bin: root.join("apache").join("bin").join("httpd.exe"),
            apache_conf: root.join("apache").join("conf").join("httpd.conf"),
            apache_logs: root.join("apache").join("logs"),
            mysql_bin: root.join("mysql").join("bin").join("mysqld.exe"),
            mysql_data: root.join("mysql").join("data"),
            mysql_ini: root.join("mysql").join("my.ini"),
            php_bin: root.join("php").join("php-cgi.exe"),
            php_ini: root.join("php").join("php.ini"),
            php_logs: root.join("php").join("logs"),
            root,
        })
    }
}

/// Validate that a path is:
/// - absolute
/// - does not escape install_dir
/// - does not traverse with ".."
/// - is not a symlink (for critical paths)
pub fn validate_critical_path(
    path: &Path,
    install_dir: &Path,
    allow_symlink: bool,
) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("path must be absolute: {}", path.display()));
    }

    // Reject any component that is ".."
    for component in path.components() {
        use std::path::Component;
        if matches!(component, Component::ParentDir) {
            return Err(format!("path traversal rejected: {}", path.display()));
        }
    }

    // Must be inside install_dir
    if !path.starts_with(install_dir) {
        return Err(format!(
            "path {} is outside install_dir {}",
            path.display(),
            install_dir.display()
        ));
    }

    // No symlinks for critical paths
    if !allow_symlink {
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if meta.file_type().is_symlink() {
                return Err(format!(
                    "symlink not allowed for critical path: {}",
                    path.display()
                ));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_relative_install_dir() {
        assert!(InstallPaths::from_install_dir(Path::new("relative/path")).is_err());
    }

    #[test]
    fn rejects_traversal() {
        let base = Path::new("C:\\ramp");
        let bad = Path::new("C:\\ramp\\..\\windows\\system32\\evil.exe");
        assert!(validate_critical_path(bad, base, false).is_err());
    }

    #[test]
    fn rejects_path_outside_install_dir() {
        let base = Path::new("C:\\ramp");
        let outside = Path::new("C:\\windows\\system32\\httpd.exe");
        assert!(validate_critical_path(outside, base, false).is_err());
    }

    #[test]
    fn accepts_valid_path() {
        let base = Path::new("C:\\ramp");
        let ok = Path::new("C:\\ramp\\apache\\bin\\httpd.exe");
        assert!(validate_critical_path(ok, base, true).is_ok());
    }
}
