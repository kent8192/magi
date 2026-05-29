use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{MagiError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub root: PathBuf,
    pub config_file: PathBuf,
    pub redis_dir: PathBuf,
    pub redis_data_dir: PathBuf,
    pub run_dir: PathBuf,
}

impl ConfigPaths {
    pub fn from_home(home: impl AsRef<Path>) -> Self {
        let root = home.as_ref().join(".magi");
        Self {
            config_file: root.join("config.toml"),
            redis_dir: root.join("redis"),
            redis_data_dir: root.join("redis").join("data"),
            run_dir: root.join("run"),
            root,
        }
    }

    pub fn from_env() -> Result<Self> {
        let home = env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            MagiError::InvalidConfig(
                "HOME is not set; magi config currently targets Unix/macOS home directories"
                    .to_string(),
            )
        })?;
        Ok(Self::from_home(home))
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct AppConfig {
    pub redis: RedisConfig,
    pub identity: IdentityConfig,
    pub ssh: SshConfig,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let paths = ConfigPaths::from_env()?;
        Self::load_from_paths(&paths)
    }

    pub fn load_from_paths(paths: &ConfigPaths) -> Result<Self> {
        if !paths.config_file.exists() {
            let config = Self::default();
            config.save_to_paths(paths)?;
            return Ok(config);
        }

        validate_config_file_permissions(&paths.config_file)?;
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path - CLI config path is derived from HOME/.magi, not request input.
        let contents = fs::read_to_string(&paths.config_file)?;
        let config = toml::from_str(&contents)?;
        Ok(config)
    }

    pub fn save_to_paths(&self, paths: &ConfigPaths) -> Result<()> {
        create_private_dir(&paths.root)?;
        create_private_dir(&paths.redis_dir)?;
        create_private_dir(&paths.redis_data_dir)?;
        create_private_dir(&paths.run_dir)?;

        let contents = toml::to_string_pretty(self)?;
        write_private_config_file(&paths.config_file, contents.as_bytes())?;
        Ok(())
    }

    pub fn get_value(&self, key: &str) -> Result<String> {
        let value = match key {
            "redis.url" => self.redis.url.as_deref().unwrap_or("").to_string(),
            "redis.bind" => self.redis.bind.clone(),
            "redis.port" => self.redis.port.to_string(),
            "identity.active_team" => self
                .identity
                .active_team
                .as_deref()
                .unwrap_or("")
                .to_string(),
            "identity.active_agent" => self
                .identity
                .active_agent
                .as_deref()
                .unwrap_or("")
                .to_string(),
            _ => {
                return Err(MagiError::NotFound(format!(
                    "unsupported config key `{key}`"
                )))
            }
        };
        Ok(value)
    }

    pub fn set_value(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            "redis.url" => self.redis.url = non_empty_value(value),
            "redis.bind" => self.redis.bind = value.to_string(),
            "redis.port" => {
                self.redis.port = parse_port(key, value)?;
            }
            "identity.active_team" => self.identity.active_team = non_empty_value(value),
            "identity.active_agent" => self.identity.active_agent = non_empty_value(value),
            _ => {
                return Err(MagiError::NotFound(format!(
                    "unsupported config key `{key}`"
                )))
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct RedisConfig {
    pub url: Option<String>,
    pub mode: RedisMode,
    pub bind: String,
    pub port: u16,
}

impl Default for RedisConfig {
    fn default() -> Self {
        Self {
            url: None,
            mode: RedisMode::Docker,
            bind: "127.0.0.1".to_string(),
            port: 6379,
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RedisMode {
    #[default]
    Docker,
    RedisServer,
    External,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct IdentityConfig {
    pub active_team: Option<String>,
    pub active_agent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct SshConfig {
    pub enabled: bool,
    pub host: String,
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

impl Default for SshConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            local_port: 6379,
            remote_host: "127.0.0.1".to_string(),
            remote_port: 6379,
        }
    }
}

pub async fn get(key: String) -> Result<()> {
    let config = AppConfig::load()?;
    println!("{}", config.get_value(&key)?);
    Ok(())
}

pub async fn set(key: String, value: String) -> Result<()> {
    let mut config = AppConfig::load()?;
    config.set_value(&key, &value)?;
    let paths = ConfigPaths::from_env()?;
    config.save_to_paths(&paths)?;
    println!("set {key}");
    Ok(())
}

fn non_empty_value(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn parse_port(key: &str, value: &str) -> Result<u16> {
    let port = value
        .parse::<u16>()
        .map_err(|_| MagiError::InvalidConfig(format!("{key} must be a TCP port")))?;
    if port == 0 {
        return Err(MagiError::InvalidConfig(format!(
            "{key} must be between 1 and 65535"
        )));
    }
    Ok(port)
}

fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    set_private_dir_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn validate_config_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(MagiError::InvalidConfig(format!(
            "unsafe permissions on {}; expected 0600 or stricter",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_config_file_permissions(_path: &Path) -> Result<()> {
    Err(non_unix_permission_error())
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Err(non_unix_permission_error())
}

#[cfg(unix)]
fn write_private_config_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let (temp_path, mut temp_file) = create_private_temp_file(path)?;
    let write_result = (|| -> Result<()> {
        temp_file.write_all(contents)?;
        temp_file.sync_all()?;
        drop(temp_file);
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temp_path, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    write_result
}

#[cfg(not(unix))]
fn write_private_config_file(_path: &Path, _contents: &[u8]) -> Result<()> {
    Err(non_unix_permission_error())
}

#[cfg(unix)]
fn create_private_temp_file(path: &Path) -> Result<(PathBuf, fs::File)> {
    use std::fs::OpenOptions;
    use std::io::ErrorKind;
    use std::os::unix::fs::OpenOptionsExt;

    let parent = path.parent().ok_or_else(|| {
        MagiError::InvalidConfig("config path has no parent directory".to_string())
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            MagiError::InvalidConfig("config file name is not valid UTF-8".to_string())
        })?;

    for attempt in 0..100 {
        let candidate = parent.join(format!(
            ".{file_name}.tmp.{}.{}",
            std::process::id(),
            attempt
        ));

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }

    Err(MagiError::InvalidConfig(
        "could not allocate private config temp file".to_string(),
    ))
}

#[cfg(not(unix))]
fn non_unix_permission_error() -> MagiError {
    MagiError::InvalidConfig(
        "magi config permission safety currently requires Unix/macOS filesystem permissions"
            .to_string(),
    )
}
