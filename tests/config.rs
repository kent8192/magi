//! Integration tests for `AppConfig` persistence and validation.
//!
//! Verifies that magi's configuration layer correctly handles the full
//! lifecycle of a `~/.magi/config.toml` file:
//!
//! * Default values match the expected localhost-Redis defaults.
//! * `save_to_paths` creates the required directory tree (`redis/`, `redis/data/`,
//!   `run/`) and writes the config file with owner-only permissions (`0o600`/`0o700`
//!   on Unix).
//! * A pre-existing symlink at the config path is atomically replaced by a real
//!   file so that the saved config cannot escape its isolation boundary.
//! * Round-trip serialisation (`save_to_paths` → `load_from_paths`) is lossless
//!   for all supported fields, including identity state.
//! * Loading a missing config auto-creates a valid default on disk.
//! * Group- or world-readable config files are rejected with `MagiError::InvalidConfig`
//!   (prevents credential leakage on shared systems).
//! * Malformed TOML produces `MagiError::TomlDeserialize`.
//! * `set_value`/`get_value` key-value accessors accept all documented keys and
//!   reject invalid values with descriptive errors.
//!
//! These tests operate entirely inside a `tempfile::TempDir` so they never
//! touch the real `~/.magi` directory and leave no state on disk after the
//! test process exits.

use std::fs;

use magi::config::{AppConfig, ConfigPaths, RedisMode};
use magi::error::MagiError;
use tempfile::TempDir;

/// Creates an isolated `ConfigPaths` rooted inside a fresh temporary directory.
///
/// Returns the `TempDir` guard alongside the paths so the caller can keep the
/// directory alive for the duration of the test (dropping `TempDir` deletes it).
fn temp_paths() -> (TempDir, ConfigPaths) {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = ConfigPaths::from_home(temp.path());
    (temp, paths)
}

#[test]
fn default_config_uses_localhost_redis() {
    let config = AppConfig::default();

    assert_eq!(config.redis.url, None);
    assert_eq!(config.redis.mode, RedisMode::Docker);
    assert_eq!(config.redis.bind, "127.0.0.1");
    assert_eq!(config.redis.port, 6379);
    assert_eq!(config.identity.active_team, None);
    assert_eq!(config.identity.active_agent, None);
    assert!(!config.ssh.enabled);
    assert_eq!(config.ssh.host, "");
    assert_eq!(config.ssh.local_port, 6379);
    assert_eq!(config.ssh.remote_host, "127.0.0.1");
    assert_eq!(config.ssh.remote_port, 6379);
}

#[test]
fn writes_config_with_private_permissions() {
    let (_temp, paths) = temp_paths();

    AppConfig::default().save_to_paths(&paths).expect("save");

    assert!(paths.redis_dir.exists());
    assert!(paths.redis_data_dir.exists());
    assert!(paths.run_dir.exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        // Mask to the lower nine permission bits so setuid/setgid/sticky do not
        // interfere with the assertion on older Linux kernels.
        let root_mode = fs::metadata(&paths.root)
            .expect("root metadata")
            .permissions()
            .mode()
            & 0o777;
        let config_mode = fs::metadata(&paths.config_file)
            .expect("config metadata")
            .permissions()
            .mode()
            & 0o777;

        // Root state directory must be accessible only by the owning user.
        assert_eq!(root_mode, 0o700);
        // Config file must be readable/writable only by the owning user.
        assert_eq!(config_mode, 0o600);
    }
}

/// Verifies that `save_to_paths` does not follow symlinks when writing the
/// config file.  A symlink pointing outside the state directory must be
/// replaced by a real private file rather than written through, preventing a
/// symlink-hijack attack where a malicious process pre-creates the path.
#[cfg(unix)]
#[test]
fn save_replaces_existing_config_symlink_with_private_file() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let (_temp, paths) = temp_paths();
    fs::create_dir_all(&paths.root).expect("create root");

    // Place a file outside the state root, then symlink the config path to it.
    let outside_file = paths.root.join("outside.toml");
    fs::write(&outside_file, "outside = true\n").expect("write outside file");
    symlink(&outside_file, &paths.config_file).expect("symlink config");

    AppConfig::default().save_to_paths(&paths).expect("save");

    let outside_contents = fs::read_to_string(&outside_file).expect("outside contents");
    let config_type = fs::symlink_metadata(&paths.config_file)
        .expect("config symlink metadata")
        .file_type();
    let config_mode = fs::metadata(&paths.config_file)
        .expect("config metadata")
        .permissions()
        .mode()
        & 0o777;

    // The outside file must be untouched — the save must not have written through the symlink.
    assert_eq!(outside_contents, "outside = true\n");
    // The config path must now be a regular file, not a symlink.
    assert!(!config_type.is_symlink());
    assert!(config_type.is_file());
    assert_eq!(config_mode, 0o600);
    // No leftover atomic-write temp files should remain in the root directory.
    assert!(!fs::read_dir(&paths.root)
        .expect("read root")
        .filter_map(|entry| entry.ok())
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".config.toml.tmp.")));
}

/// Confirms that all config fields survive a save/load round trip without loss
/// or corruption, including non-default identity fields.
#[test]
fn round_trips_config_including_identity() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
    config.redis.url = Some("redis://127.0.0.1:6380".to_string());
    config.redis.mode = RedisMode::External;
    config.redis.port = 6380;
    config.identity.active_team = Some("core".to_string());
    config.identity.active_agent = Some("alice".to_string());

    config.save_to_paths(&paths).expect("save");
    let loaded = AppConfig::load_from_paths(&paths).expect("load");

    assert_eq!(loaded, config);
}

#[test]
fn load_creates_default_config_when_missing() {
    let (_temp, paths) = temp_paths();

    let loaded = AppConfig::load_from_paths(&paths).expect("load");

    assert_eq!(loaded, AppConfig::default());
    // A config file must have been written so subsequent loads find it on disk.
    assert!(paths.config_file.exists());
}

/// On Unix, config files with group or world read bits set are rejected to
/// prevent credential leakage on shared-user systems.
#[cfg(unix)]
#[test]
fn load_rejects_config_toml_with_group_world_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let (_temp, paths) = temp_paths();
    AppConfig::default().save_to_paths(&paths).expect("save");
    // Widen permissions to 0o644 (group-readable) to trigger the safety check.
    fs::set_permissions(&paths.config_file, fs::Permissions::from_mode(0o644))
        .expect("set permissions");

    let error = AppConfig::load_from_paths(&paths).expect_err("unsafe permissions should fail");

    assert!(
        matches!(error, MagiError::InvalidConfig(message) if message.contains("unsafe permissions"))
    );
}

#[test]
fn load_rejects_invalid_toml() {
    let (_temp, paths) = temp_paths();
    fs::create_dir_all(&paths.root).expect("create root");
    // Write deliberately malformed TOML (unclosed array) to exercise the parse-error path.
    fs::write(&paths.config_file, "not = [valid").expect("write invalid toml");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Apply correct permissions so the permission check passes and TOML parsing is reached.
        fs::set_permissions(&paths.root, fs::Permissions::from_mode(0o700)).expect("root perms");
        fs::set_permissions(&paths.config_file, fs::Permissions::from_mode(0o600))
            .expect("config perms");
    }

    let error = AppConfig::load_from_paths(&paths).expect_err("invalid toml should fail");

    assert!(matches!(error, MagiError::TomlDeserialize(_)));
}

/// Verifies that `set_value` rejects a non-numeric string for `redis.port` and
/// leaves the existing value unchanged.
#[test]
fn set_redis_port_rejects_invalid_value() {
    let mut config = AppConfig::default();

    let error = config
        .set_value("redis.port", "not-a-port")
        .expect_err("invalid port should fail");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("redis.port")));
    // The original port must be preserved after a failed set.
    assert_eq!(config.redis.port, 6379);
}

/// Exercises every documented `set_value`/`get_value` key to confirm that the
/// accessor layer covers the full public key surface.
#[test]
fn set_get_supported_keys() {
    let mut config = AppConfig::default();

    config
        .set_value("redis.url", "redis://localhost:6380")
        .expect("set redis.url");
    config
        .set_value("redis.bind", "0.0.0.0")
        .expect("set redis.bind");
    config
        .set_value("redis.port", "6380")
        .expect("set redis.port");
    config
        .set_value("identity.active_team", "core")
        .expect("set active team");
    config
        .set_value("identity.active_agent", "alice")
        .expect("set active agent");

    assert_eq!(
        config.get_value("redis.url").expect("get redis.url"),
        "redis://localhost:6380"
    );
    assert_eq!(
        config.get_value("redis.bind").expect("get redis.bind"),
        "0.0.0.0"
    );
    assert_eq!(
        config.get_value("redis.port").expect("get redis.port"),
        "6380"
    );
    assert_eq!(
        config
            .get_value("identity.active_team")
            .expect("get active team"),
        "core"
    );
    assert_eq!(
        config
            .get_value("identity.active_agent")
            .expect("get active agent"),
        "alice"
    );
}
