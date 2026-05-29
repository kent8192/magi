use std::fs;

use magi::config::{AppConfig, ConfigPaths, RedisMode};
use magi::error::MagiError;
use tempfile::TempDir;

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

        assert_eq!(root_mode, 0o700);
        assert_eq!(config_mode, 0o600);
    }
}

#[cfg(unix)]
#[test]
fn save_replaces_existing_config_symlink_with_private_file() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let (_temp, paths) = temp_paths();
    fs::create_dir_all(&paths.root).expect("create root");

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

    assert_eq!(outside_contents, "outside = true\n");
    assert!(!config_type.is_symlink());
    assert!(config_type.is_file());
    assert_eq!(config_mode, 0o600);
    assert!(!fs::read_dir(&paths.root)
        .expect("read root")
        .filter_map(|entry| entry.ok())
        .any(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with(".config.toml.tmp.")));
}

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
    assert!(paths.config_file.exists());
}

#[cfg(unix)]
#[test]
fn load_rejects_config_toml_with_group_world_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let (_temp, paths) = temp_paths();
    AppConfig::default().save_to_paths(&paths).expect("save");
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
    fs::write(&paths.config_file, "not = [valid").expect("write invalid toml");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&paths.root, fs::Permissions::from_mode(0o700)).expect("root perms");
        fs::set_permissions(&paths.config_file, fs::Permissions::from_mode(0o600))
            .expect("config perms");
    }

    let error = AppConfig::load_from_paths(&paths).expect_err("invalid toml should fail");

    assert!(matches!(error, MagiError::TomlDeserialize(_)));
}

#[test]
fn set_redis_port_rejects_invalid_value() {
    let mut config = AppConfig::default();

    let error = config
        .set_value("redis.port", "not-a-port")
        .expect_err("invalid port should fail");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("redis.port")));
    assert_eq!(config.redis.port, 6379);
}

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
