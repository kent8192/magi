use std::fs;

use magi::config::{AppConfig, ConfigPaths};
use magi::error::MagiError;
use magi::install::{install_binary_from_path, InstallPaths};
use magi::ssh::{build_ssh_start_plan, build_ssh_stop_plan, ssh_pid_file};
use tempfile::TempDir;

fn temp_paths() -> (TempDir, ConfigPaths) {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = ConfigPaths::from_home(temp.path());
    (temp, paths)
}

#[test]
fn install_paths_use_user_requested_layouts() {
    let home = std::path::Path::new("/tmp/home");
    let paths = InstallPaths::from_home(home);

    assert_eq!(paths.skill_bin, home.join(".agents/skills/magi/bin/magi"));
    assert_eq!(paths.local_cli, home.join(".local/bin/magi"));
    assert_eq!(ConfigPaths::from_home(home).root, home.join(".magi"));
}

#[test]
fn install_binary_copies_to_skill_bin_and_local_cli_with_executable_mode() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("magi-source");
    fs::write(&source, b"binary").expect("write source");

    let installed = install_binary_from_path(temp.path(), &source).expect("install");

    assert_eq!(fs::read(&installed.skill_bin).unwrap(), b"binary");
    assert_eq!(fs::read(&installed.local_cli).unwrap(), b"binary");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let skill_mode = fs::metadata(&installed.skill_bin)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        let local_mode = fs::metadata(&installed.local_cli)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(skill_mode, 0o755);
        assert_eq!(local_mode, 0o755);
    }
}

#[cfg(unix)]
#[test]
fn install_binary_replaces_local_cli_symlink_without_touching_target() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("magi-source");
    let outside = temp.path().join("outside");
    fs::write(&source, b"new").expect("write source");
    fs::write(&outside, b"old").expect("write outside");

    let paths = InstallPaths::from_home(temp.path());
    fs::create_dir_all(paths.local_cli.parent().unwrap()).expect("create local dir");
    symlink(&outside, &paths.local_cli).expect("symlink local cli");

    install_binary_from_path(temp.path(), &source).expect("install");

    assert_eq!(fs::read(&outside).unwrap(), b"old");
    assert_eq!(fs::read(&paths.local_cli).unwrap(), b"new");
    assert!(!fs::symlink_metadata(&paths.local_cli)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[test]
fn ssh_start_plan_uses_configured_port_forward_without_password_leak() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
    config.ssh.enabled = true;
    config.ssh.host = "ops@example.com".to_string();
    config.ssh.local_port = 6380;
    config.ssh.remote_host = "127.0.0.1".to_string();
    config.ssh.remote_port = 6379;

    let plan = build_ssh_start_plan(&config, &paths).expect("plan");

    assert_eq!(plan.program, "ssh");
    assert!(plan.args.contains(&"-N".to_string()));
    assert!(plan.args.contains(&"-L".to_string()));
    assert!(plan.args.contains(&"6380:127.0.0.1:6379".to_string()));
    assert!(plan.args.contains(&"ops@example.com".to_string()));
    assert_eq!(plan.pid_file, ssh_pid_file(&paths));
    assert!(!plan.args.iter().any(|arg| arg.contains("password")));
}

#[test]
fn ssh_start_plan_rejects_missing_host() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
    config.ssh.enabled = true;

    let error = build_ssh_start_plan(&config, &paths).expect_err("host required");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("ssh.host")));
}

#[test]
fn ssh_stop_plan_targets_pid_file() {
    let (_temp, paths) = temp_paths();

    let plan = build_ssh_stop_plan(&paths);

    assert_eq!(plan.program, "kill");
    assert_eq!(plan.pid_file, ssh_pid_file(&paths));
}
