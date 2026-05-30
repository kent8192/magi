//! Persistent configuration and on-disk state layout for the `magi` CLI.
//!
//! `magi` keeps all of its per-user state under `~/.magi`. This module owns
//! that directory layout and the typed representation of the TOML config file
//! stored there. It is responsible for:
//!
//! - Resolving the `~/.magi` directory tree from the `HOME` environment
//!   variable (see `ConfigPaths`).
//! - Loading, defaulting, and saving the `AppConfig` document
//!   (`~/.magi/config.toml`), which records Redis connection settings, the
//!   active identity (team / agent), and SSH tunnel preferences.
//! - Reading and writing individual config keys via the `magi config get` /
//!   `magi config set` CLI subcommands (see `get` and `set`).
//! - Enforcing private (owner-only) filesystem permissions on the config file
//!   and the state directories, so that secrets such as a Redis URL are not
//!   world-readable.
//!
//! Within the broader CLI, this module is the single source of truth for
//! "where state lives" and "what the user configured". Other subsystems (the
//! managed Redis server lifecycle, identity/team membership, the REPL, watch
//! mode, and the SSH helpers) read an `AppConfig` loaded here rather than
//! touching the filesystem layout directly.
//!
//! ## Platform support
//!
//! The permission-hardening paths are Unix/macOS specific. On non-Unix
//! targets the private read/write helpers deliberately return an error
//! (see `non_unix_permission_error`), because the security model relies on
//! POSIX file-mode bits.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{MagiError, Result};

/// Absolute paths for every component of the `~/.magi` state directory.
///
/// All paths are derived from a single home directory, so the whole tree can
/// be relocated (e.g. for tests) by constructing this struct from a temporary
/// home via `ConfigPaths::from_home`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    /// The `~/.magi` root directory that contains all other state.
    pub root: PathBuf,
    /// The `~/.magi/config.toml` file backing `AppConfig`.
    pub config_file: PathBuf,
    /// The `~/.magi/redis` directory for the managed/embedded Redis server.
    pub redis_dir: PathBuf,
    /// The `~/.magi/redis/data` directory holding Redis persistence files.
    pub redis_data_dir: PathBuf,
    /// The `~/.magi/run` directory for runtime artifacts (e.g. PID/socket files).
    pub run_dir: PathBuf,
}

impl ConfigPaths {
    /// Build the full `~/.magi` path tree rooted at an explicit home directory.
    ///
    /// This is the deterministic, side-effect-free constructor: it only joins
    /// path components and never touches the filesystem. Tests use it to point
    /// the state tree at a temporary directory.
    pub fn from_home(home: impl AsRef<Path>) -> Self {
        // All state lives under a single hidden `.magi` directory in `$HOME`.
        let root = home.as_ref().join(".magi");
        Self {
            config_file: root.join("config.toml"),
            redis_dir: root.join("redis"),
            redis_data_dir: root.join("redis").join("data"),
            run_dir: root.join("run"),
            // `root` is moved last so the joins above can borrow it first.
            root,
        }
    }

    /// Resolve the `~/.magi` path tree from the `HOME` environment variable.
    ///
    /// # Errors
    ///
    /// Returns `MagiError::InvalidConfig` when `HOME` is unset. The current
    /// design targets Unix/macOS home directories, so a missing `HOME` is
    /// treated as an unsupported environment rather than falling back.
    pub fn from_env() -> Result<Self> {
        // `HOME` is the sole anchor for the state directory; without it we
        // cannot safely guess a location for user secrets.
        let home = env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
            MagiError::InvalidConfig(
                "HOME is not set; magi config currently targets Unix/macOS home directories"
                    .to_string(),
            )
        })?;
        Ok(Self::from_home(home))
    }
}

/// The complete `magi` configuration document, mirrored to
/// `~/.magi/config.toml`.
///
/// `#[serde(default)]` makes every section optional in the TOML file: a
/// missing section deserializes to its `Default`, so old config files remain
/// loadable as new sections are added.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct AppConfig {
    /// Redis connection and server-management settings.
    pub redis: RedisConfig,
    /// The currently active team and agent identity.
    pub identity: IdentityConfig,
    /// SSH tunnel settings for reaching a remote Redis instance.
    pub ssh: SshConfig,
}

impl AppConfig {
    /// Load the config from the default `~/.magi` location.
    ///
    /// Resolves the path tree from `HOME` and delegates to
    /// `AppConfig::load_from_paths`.
    ///
    /// # Errors
    ///
    /// Propagates errors from `ConfigPaths::from_env` (missing `HOME`) and
    /// from `AppConfig::load_from_paths` (permission, I/O, or parse errors).
    pub fn load() -> Result<Self> {
        let paths = ConfigPaths::from_env()?;
        Self::load_from_paths(&paths)
    }

    /// Load the config from an explicit set of `ConfigPaths`.
    ///
    /// If the config file does not yet exist, a default config is created,
    /// persisted to disk (which also creates the state directories), and
    /// returned. This makes first-run behavior transparent: the caller always
    /// receives a usable config.
    ///
    /// # Errors
    ///
    /// Returns an error if the existing file has unsafe permissions
    /// (see `validate_config_file_permissions`), cannot be read, or fails to
    /// parse as TOML. On first run, also propagates save failures.
    pub fn load_from_paths(paths: &ConfigPaths) -> Result<Self> {
        // First run: no file yet, so materialize defaults and persist them.
        if !paths.config_file.exists() {
            let config = Self::default();
            config.save_to_paths(paths)?;
            return Ok(config);
        }

        // Refuse to read a config file that is group/world accessible, since
        // it may contain a Redis URL with embedded credentials.
        validate_config_file_permissions(&paths.config_file)?;
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path - CLI config path is derived from HOME/.magi, not request input.
        let contents = fs::read_to_string(&paths.config_file)?;
        let config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Persist this config to disk under the given `ConfigPaths`.
    ///
    /// Creates the full `~/.magi` directory tree with private (`0700`)
    /// permissions, then writes `config.toml` atomically with `0600`
    /// permissions (see `write_private_config_file`).
    ///
    /// # Errors
    ///
    /// Returns an error if any directory cannot be created with private
    /// permissions, if serialization to TOML fails, or if the atomic write
    /// fails.
    pub fn save_to_paths(&self, paths: &ConfigPaths) -> Result<()> {
        // Ensure every component of the state tree exists and is owner-only
        // before any data is written into it.
        create_private_dir(&paths.root)?;
        create_private_dir(&paths.redis_dir)?;
        create_private_dir(&paths.redis_data_dir)?;
        create_private_dir(&paths.run_dir)?;

        let contents = toml::to_string_pretty(self)?;
        write_private_config_file(&paths.config_file, contents.as_bytes())?;
        Ok(())
    }

    /// Read a single config value by its dotted key (e.g. `redis.port`).
    ///
    /// Supports the keys exposed via `magi config get`:
    /// `redis.url`, `redis.bind`, `redis.port`, `identity.active_team`, and
    /// `identity.active_agent`. Unset optional values are returned as the
    /// empty string.
    ///
    /// # Errors
    ///
    /// Returns `MagiError::NotFound` for any unsupported key.
    pub fn get_value(&self, key: &str) -> Result<String> {
        let value = match key {
            // Optional values render as an empty string when unset, matching
            // the convention of a shell-style `config get`.
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

    /// Set a single config value by its dotted key (e.g. `redis.port`).
    ///
    /// Mutates only the in-memory config; the caller is responsible for
    /// persisting via `AppConfig::save_to_paths`. Empty strings clear
    /// optional fields back to `None` (see `non_empty_value`).
    ///
    /// # Errors
    ///
    /// Returns `MagiError::InvalidConfig` when `redis.port` is not a valid
    /// non-zero TCP port, or `MagiError::NotFound` for an unsupported key.
    pub fn set_value(&mut self, key: &str, value: &str) -> Result<()> {
        match key {
            // An empty value clears the override so the URL falls back to the
            // bind/port-derived connection elsewhere in the CLI.
            "redis.url" => self.redis.url = non_empty_value(value),
            "redis.bind" => self.redis.bind = value.to_string(),
            "redis.port" => {
                // Reject non-numeric and zero ports up front (fail early).
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

/// How `magi` connects to (and optionally manages) Redis.
///
/// The messaging layer needs a Redis endpoint; this section describes both the
/// connection target and which lifecycle `RedisMode` provides it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct RedisConfig {
    /// Explicit Redis connection URL. When set, it overrides `bind`/`port`.
    /// May contain credentials, which is why the config file is kept private.
    pub url: Option<String>,
    /// Which provisioning strategy supplies the Redis server (see `RedisMode`).
    pub mode: RedisMode,
    /// Interface address Redis binds to when `magi` manages the server.
    pub bind: String,
    /// TCP port for the Redis server / connection.
    pub port: u16,
}

impl Default for RedisConfig {
    /// Defaults to a Docker-managed Redis bound to loopback on the standard
    /// Redis port (`127.0.0.1:6379`), with no explicit URL override.
    fn default() -> Self {
        Self {
            url: None,
            mode: RedisMode::Docker,
            bind: "127.0.0.1".to_string(),
            port: 6379,
        }
    }
}

/// The strategy `magi` uses to obtain a running Redis server.
///
/// Serialized as kebab-case in TOML (e.g. `redis-server`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RedisMode {
    /// Run Redis inside a Docker container managed by `magi` (the default).
    #[default]
    Docker,
    /// Launch a local `redis-server` binary as a managed child process.
    RedisServer,
    /// Connect to an externally provisioned Redis that `magi` does not manage.
    External,
}

/// The active messaging identity: which team and agent the CLI acts as.
///
/// Both fields are optional because a fresh install has no identity until the
/// invite-based onboarding flow selects one.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct IdentityConfig {
    /// Name of the currently selected team, or `None` if unset.
    pub active_team: Option<String>,
    /// Name of the currently selected agent, or `None` if unset.
    pub active_agent: Option<String>,
}

/// SSH tunnel settings for reaching a Redis instance on a remote host.
///
/// When `enabled` is true, the SSH helpers forward a
/// local port through `host` to the remote Redis endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct SshConfig {
    /// Whether SSH tunneling is active.
    pub enabled: bool,
    /// SSH destination (`user@host` or an `ssh_config` alias) to tunnel through.
    pub host: String,
    /// Local port that forwards into the tunnel.
    pub local_port: u16,
    /// Redis host as seen from the remote side of the tunnel.
    pub remote_host: String,
    /// Redis port on the remote side of the tunnel.
    pub remote_port: u16,
}

impl Default for SshConfig {
    /// Defaults to a disabled tunnel that, when enabled, would forward
    /// `localhost:6379` to `127.0.0.1:6379` on the (yet unspecified) host.
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

/// Implements the `magi config get <key>` subcommand.
///
/// Loads the config and prints the value of `key` to stdout, followed by a
/// newline.
///
/// # Errors
///
/// Propagates load errors and `MagiError::NotFound` for an unknown key.
pub async fn get(key: String) -> Result<()> {
    let config = AppConfig::load()?;
    println!("{}", config.get_value(&key)?);
    Ok(())
}

/// Implements the `magi config set <key> <value>` subcommand.
///
/// Loads the config, applies the change in memory, and persists it back to
/// `~/.magi/config.toml`, then prints a confirmation.
///
/// # Errors
///
/// Propagates load, validation (e.g. invalid port), and save errors.
pub async fn set(key: String, value: String) -> Result<()> {
    let mut config = AppConfig::load()?;
    config.set_value(&key, &value)?;
    // Re-resolve the paths to write back to the same `~/.magi` location.
    let paths = ConfigPaths::from_env()?;
    config.save_to_paths(&paths)?;
    println!("set {key}");
    Ok(())
}

/// Map a raw string to an optional value, treating empty input as "unset".
///
/// Used so that `magi config set <key> ""` clears an optional field rather
/// than storing an empty string.
fn non_empty_value(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// Parse and validate a TCP port string for a given config `key`.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` when `value` is not a `u16` or is `0`
/// (port `0` is reserved and not a valid endpoint). The `key` is included in
/// the message to identify which setting was invalid.
fn parse_port(key: &str, value: &str) -> Result<u16> {
    // A `u16` already caps the upper bound at 65535; we only need to reject
    // non-numeric input and the reserved port 0.
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

/// Create a directory (and parents) and lock it down to owner-only access.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or its permissions
/// cannot be set to private (see `set_private_dir_permissions`).
fn create_private_dir(path: &Path) -> Result<()> {
    // Create the directory tree first, then tighten permissions on the leaf.
    fs::create_dir_all(path)?;
    set_private_dir_permissions(path)?;
    Ok(())
}

/// Reject a config file readable or writable by group/other (Unix only).
///
/// The config may contain a Redis URL with credentials, so it must be at most
/// `0600`. Any bit in the group/other classes (`0o077`) is treated as unsafe.
///
/// # Errors
///
/// Returns an I/O error if the file's metadata cannot be read, or
/// `MagiError::InvalidConfig` if the mode is looser than `0600`.
#[cfg(unix)]
fn validate_config_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // Mask off type/sticky bits and keep only the permission triad.
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    // `0o077` covers all group and other read/write/execute bits; any of them
    // being set means the file is too permissive.
    if mode & 0o077 != 0 {
        return Err(MagiError::InvalidConfig(format!(
            "unsafe permissions on {}; expected 0600 or stricter",
            path.display()
        )));
    }
    Ok(())
}

/// Non-Unix stub: permission validation is unsupported off POSIX filesystems.
///
/// # Errors
///
/// Always returns `non_unix_permission_error`.
#[cfg(not(unix))]
fn validate_config_file_permissions(_path: &Path) -> Result<()> {
    Err(non_unix_permission_error())
}

/// Set a state directory to owner-only (`0700`) permissions (Unix only).
///
/// # Errors
///
/// Returns an I/O error if the mode cannot be applied.
#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // `0700`: only the owner may read, write, or traverse the directory.
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Non-Unix stub: directory permission hardening is unsupported off POSIX.
///
/// # Errors
///
/// Always returns `non_unix_permission_error`.
#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Err(non_unix_permission_error())
}

/// Atomically write `contents` to `path` with private (`0600`) permissions
/// (Unix only).
///
/// The write goes to a freshly created private temp file in the same
/// directory, which is then `rename`d over the target. Same-directory rename
/// is atomic on POSIX filesystems, so a reader never observes a partially
/// written config and a crash mid-write cannot corrupt the existing file.
///
/// # Errors
///
/// Returns an error if the temp file cannot be created, written, fsynced,
/// permission-set, or renamed into place. On any failure the temp file is
/// best-effort removed before the error is returned.
#[cfg(unix)]
fn write_private_config_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    // Stage the new contents in a sibling temp file opened with `0600`.
    let (temp_path, mut temp_file) = create_private_temp_file(path)?;
    // Run the write-and-swap as a closure so we can clean up the temp file on
    // any error via a single `is_err()` check below.
    let write_result = (|| -> Result<()> {
        temp_file.write_all(contents)?;
        // Flush to disk before the rename so the swapped-in file is durable.
        temp_file.sync_all()?;
        // Close the handle before renaming/repermissioning the path.
        drop(temp_file);
        fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600))?;
        // Atomic replace: the target now points at the fully written temp file.
        fs::rename(&temp_path, path)?;
        // Re-assert `0600` on the final path in case the rename target
        // inherited a looser mode from a pre-existing file.
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    })();

    // Best-effort cleanup of the orphaned temp file when the swap failed; the
    // original error is still propagated below.
    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    write_result
}

/// Non-Unix stub: atomic private writes are unsupported off POSIX filesystems.
///
/// # Errors
///
/// Always returns `non_unix_permission_error`.
#[cfg(not(unix))]
fn write_private_config_file(_path: &Path, _contents: &[u8]) -> Result<()> {
    Err(non_unix_permission_error())
}

/// Create a uniquely named, owner-only temp file beside the target `path`.
///
/// The candidate name embeds the process id and an attempt counter so that
/// concurrent or repeated writers do not collide. The file is opened with
/// `create_new` (O_EXCL) and mode `0600`, guaranteeing it did not pre-exist
/// and is private from creation.
///
/// # Returns
///
/// On success, the path of the created temp file and its open handle.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` if `path` has no parent or a
/// non-UTF-8 file name, or if 100 unique-name attempts all collide. Any other
/// I/O error during creation is propagated.
#[cfg(unix)]
fn create_private_temp_file(path: &Path) -> Result<(PathBuf, fs::File)> {
    use std::fs::OpenOptions;
    use std::io::ErrorKind;
    use std::os::unix::fs::OpenOptionsExt;

    // The temp file must live in the same directory as the target so the later
    // rename stays within one filesystem (a cross-device rename is not atomic).
    let parent = path.parent().ok_or_else(|| {
        MagiError::InvalidConfig("config path has no parent directory".to_string())
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            MagiError::InvalidConfig("config file name is not valid UTF-8".to_string())
        })?;

    // Try a bounded number of distinct candidate names to tolerate races where
    // another writer already grabbed a given name.
    for attempt in 0..100 {
        // Hidden dotfile name keyed by pid + attempt to avoid clashing with
        // the real config and with parallel writers.
        let candidate = parent.join(format!(
            ".{file_name}.tmp.{}.{}",
            std::process::id(),
            attempt
        ));

        // `create_new` (O_EXCL) fails if the file exists, so success proves we
        // created a fresh file; `mode(0o600)` makes it private atomically.
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            // Name already taken: retry with the next attempt counter.
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            // Any other error is fatal for this write.
            Err(error) => return Err(error.into()),
        }
    }

    // Exhausting all attempts indicates pathological contention or leftover
    // temp files; surface it rather than looping forever.
    Err(MagiError::InvalidConfig(
        "could not allocate private config temp file".to_string(),
    ))
}

/// Build the standard error returned by the non-Unix permission stubs.
///
/// `magi`'s config security model depends on POSIX file-mode bits, so on
/// non-Unix targets the private-permission operations refuse to proceed rather
/// than silently writing world-readable state.
#[cfg(not(unix))]
fn non_unix_permission_error() -> MagiError {
    MagiError::InvalidConfig(
        "magi config permission safety currently requires Unix/macOS filesystem permissions"
            .to_string(),
    )
}
