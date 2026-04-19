use crate::paths::InstallPaths;
use crate::state::{ApacheConfig, MysqlConfig, PhpConfig, RampConfig};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

/// On-disk TOML representation (user-editable).
#[derive(Debug, Serialize, Deserialize)]
struct TomlRoot {
    install_dir: PathBuf,
    apache: TomlApache,
    mysql: TomlMysql,
    #[serde(default)]
    php: TomlPhp,
}

#[derive(Debug, Serialize, Deserialize)]
struct TomlApache {
    port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
struct TomlMysql {
    port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
struct TomlPhp {
    port: u16,
}

impl Default for TomlPhp {
    fn default() -> Self {
        Self { port: 9000 }
    }
}

/// Load and validate ramp.toml from install_dir.
pub fn load_config(install_dir: &Path) -> Result<RampConfig, String> {
    let paths = InstallPaths::from_install_dir(install_dir)?;
    let raw = std::fs::read_to_string(&paths.config)
        .map_err(|e| format!("cannot read ramp.toml: {e}"))?;
    let doc: TomlRoot = toml::from_str(&raw).map_err(|e| format!("ramp.toml parse error: {e}"))?;
    validate_and_build(doc, install_dir)
}

/// Write a default ramp.toml if none exists. Does not overwrite.
pub fn write_default_config(install_dir: &Path) -> Result<(), String> {
    let paths = InstallPaths::from_install_dir(install_dir)?;
    if paths.config.exists() {
        return Ok(());
    }
    let default = format!(
        r#"install_dir = "{}"

[apache]
port = 80

[mysql]
port = 3306

[php]
port = 9000
"#,
        install_dir.display().to_string().replace('\\', "\\\\")
    );
    atomic_write(&paths.config, default.as_bytes())
}

fn validate_and_build(doc: TomlRoot, install_dir: &Path) -> Result<RampConfig, String> {
    let paths = InstallPaths::from_install_dir(install_dir)?;

    if doc.apache.port == 0 {
        return Err(format!("invalid apache.port: {}", doc.apache.port));
    }
    if doc.mysql.port == 0 {
        return Err(format!("invalid mysql.port: {}", doc.mysql.port));
    }
    if doc.php.port == 0 {
        return Err(format!("invalid php.port: {}", doc.php.port));
    }
    if doc.apache.port == doc.mysql.port {
        return Err("apache.port and mysql.port must be different".into());
    }
    if doc.apache.port == doc.php.port {
        return Err("apache.port and php.port must be different".into());
    }
    if doc.mysql.port == doc.php.port {
        return Err("mysql.port and php.port must be different".into());
    }

    Ok(RampConfig {
        install_dir: install_dir.to_path_buf(),
        apache: ApacheConfig {
            port: doc.apache.port,
            bin: paths.apache_bin,
            conf: paths.apache_conf,
        },
        mysql: MysqlConfig {
            port: doc.mysql.port,
            bin: paths.mysql_bin,
            data_dir: paths.mysql_data,
            ini: paths.mysql_ini,
        },
        php: PhpConfig {
            port: doc.php.port,
            bin: paths.php_bin,
            ini: paths.php_ini,
        },
    })
}

/// Atomic write: temp file → fsync → rename. Never corrupts the target.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<(), String> {
    let dir = path.parent().ok_or("path has no parent")?;
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("cannot create dir {}: {e}", dir.display()))?;

    let tmp = path.with_extension("tmp");
    {
        let mut f =
            std::fs::File::create(&tmp).map_err(|e| format!("cannot create temp file: {e}"))?;
        f.write_all(data)
            .map_err(|e| format!("write failed: {e}"))?;
        f.flush().map_err(|e| format!("flush failed: {e}"))?;
        f.sync_all().map_err(|e| format!("fsync failed: {e}"))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("atomic rename failed: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_toml(dir: &Path, content: &str) {
        std::fs::write(dir.join("ramp.toml"), content).unwrap();
    }

    #[test]
    fn load_valid_config() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_toml(
            dir,
            &format!(
                r#"install_dir = "{}"
[apache]
port = 8080
[mysql]
port = 3306
[php]
port = 9000
"#,
                dir.display().to_string().replace('\\', "\\\\")
            ),
        );
        let cfg = load_config(dir).unwrap();
        assert_eq!(cfg.apache.port, 8080);
        assert_eq!(cfg.mysql.port, 3306);
        assert_eq!(cfg.php.port, 9000);
    }

    #[test]
    fn load_config_defaults_php_port_when_absent() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_toml(
            dir,
            &format!(
                r#"install_dir = "{}"
[apache]
port = 8080
[mysql]
port = 3306
"#,
                dir.display().to_string().replace('\\', "\\\\")
            ),
        );
        let cfg = load_config(dir).unwrap();
        assert_eq!(cfg.php.port, 9000);
    }

    #[test]
    fn rejects_duplicate_ports() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_toml(
            dir,
            &format!(
                r#"install_dir = "{}"
[apache]
port = 80
[mysql]
port = 80
"#,
                dir.display().to_string().replace('\\', "\\\\")
            ),
        );
        assert!(load_config(dir).is_err());
    }

    #[test]
    fn rejects_apache_php_port_clash() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        write_toml(
            dir,
            &format!(
                r#"install_dir = "{}"
[apache]
port = 9000
[mysql]
port = 3306
[php]
port = 9000
"#,
                dir.display().to_string().replace('\\', "\\\\")
            ),
        );
        assert!(load_config(dir).is_err());
    }

    #[test]
    fn atomic_write_round_trip() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.toml");
        atomic_write(&path, b"hello").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        // Second write replaces atomically
        atomic_write(&path, b"world").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"world");
        // No .tmp file left behind
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn write_default_does_not_overwrite_existing() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        std::fs::write(dir.join("ramp.toml"), b"original").unwrap();
        write_default_config(dir).unwrap();
        assert_eq!(std::fs::read(dir.join("ramp.toml")).unwrap(), b"original");
    }
}
