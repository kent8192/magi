//! Integration tests for the installer and SSH tunnel helper.
//!
//! These tests verify that:
//! - `InstallPaths` and `ConfigPaths` produce the directory layout described in
//!   the project spec (`~/.agents/skills/magi/bin/magi`, `~/.local/bin/magi`,
//!   and `~/.magi` for state).
//! - `install_binary_from_path` copies the binary to both install locations,
//!   sets executable permissions (`0o755` on Unix), and correctly replaces a
//!   pre-existing symlink at `local_cli` without modifying the symlink target.
//! - `build_ssh_start_plan` produces a well-formed `ssh -N -L` port-forward
//!   command and rejects a config that is missing the required `ssh.host` field.
//! - `build_ssh_stop_plan` targets the PID file written by the SSH tunnel
//!   lifecycle code so the managed tunnel can be cleanly shut down.
//!
//! No live Redis server or SSH daemon is required; all tests operate on the
//! local filesystem using `tempfile::TempDir` for isolation.

use std::fs;

use magi::config::{AppConfig, ConfigPaths};
use magi::error::MagiError;
use magi::install::{install_binary_from_path, InstallPaths};
use magi::ssh::{build_ssh_start_plan, build_ssh_stop_plan, ssh_pid_file};
use tempfile::TempDir;

// Creates a temporary home directory and derives `ConfigPaths` from it.
// Keeping the `TempDir` alive for the duration of the test ensures the
// directory is not deleted while paths derived from it are still in use.
fn temp_paths() -> (TempDir, ConfigPaths) {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = ConfigPaths::from_home(temp.path());
    (temp, paths)
}

/// Confirms the hard-coded directory layout matches what the project spec requires.
#[test]
fn install_paths_use_user_requested_layouts() {
    let home = std::path::Path::new("/tmp/home");
    let paths = InstallPaths::from_home(home);

    assert_eq!(paths.skill_bin, home.join(".agents/skills/magi/bin/magi"));
    assert_eq!(paths.local_cli, home.join(".local/bin/magi"));
    assert_eq!(ConfigPaths::from_home(home).root, home.join(".magi"));
}

/// Verifies that the binary reaches both install locations with identical content
/// and that the Unix permission bits are set to `0o755` (owner-execute + world-read/execute).
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
        // Mask to lower nine permission bits so we are not misled by the file-type bits.
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

/// Ensures that a pre-existing symlink at `local_cli` is atomically replaced by a
/// regular file rather than being followed, so the original symlink target is not
/// overwritten and no dangling symlink is left behind.
#[cfg(unix)]
#[test]
fn install_binary_replaces_local_cli_symlink_without_touching_target() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().expect("tempdir");
    let source = temp.path().join("magi-source");
    // A file outside the install tree that the symlink currently points to.
    let outside = temp.path().join("outside");
    fs::write(&source, b"new").expect("write source");
    fs::write(&outside, b"old").expect("write outside");

    let paths = InstallPaths::from_home(temp.path());
    fs::create_dir_all(paths.local_cli.parent().unwrap()).expect("create local dir");
    // Plant an existing symlink so the installer must handle the replace-symlink path.
    symlink(&outside, &paths.local_cli).expect("symlink local cli");

    install_binary_from_path(temp.path(), &source).expect("install");

    // The target of the old symlink must be untouched.
    assert_eq!(fs::read(&outside).unwrap(), b"old");
    // `local_cli` must now contain the new binary content.
    assert_eq!(fs::read(&paths.local_cli).unwrap(), b"new");
    // `local_cli` must be a regular file, not a symlink.
    assert!(!fs::symlink_metadata(&paths.local_cli)
        .unwrap()
        .file_type()
        .is_symlink());
}

/// Verifies the SSH port-forward plan structure: `ssh -N -L <local>:<remote-host>:<remote-port>
/// <host>`. Also asserts that no argument contains the string `"password"` to guard against
/// accidentally embedding credentials in the argv that would be visible in the process table.
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
    // `-N` suppresses remote command execution; the tunnel runs in the foreground.
    assert!(plan.args.contains(&"-N".to_string()));
    assert!(plan.args.contains(&"-L".to_string()));
    // The `-L` argument encodes `local_port:remote_host:remote_port`.
    assert!(plan.args.contains(&"6380:127.0.0.1:6379".to_string()));
    assert!(plan.args.contains(&"ops@example.com".to_string()));
    // The PID file path must match what the stop plan will later read.
    assert_eq!(plan.pid_file, ssh_pid_file(&paths));
    assert!(!plan.args.iter().any(|arg| arg.contains("password")));
}

/// Confirms that `build_ssh_start_plan` returns `MagiError::InvalidConfig` when
/// `ssh.host` is not set, and that the error message names the missing field so
/// users can identify the misconfiguration quickly.
#[test]
fn ssh_start_plan_rejects_missing_host() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
    config.ssh.enabled = true;

    let error = build_ssh_start_plan(&config, &paths).expect_err("host required");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("ssh.host")));
}

/// Checks that the stop plan invokes `kill` and references the same PID file
/// that `build_ssh_start_plan` writes, ensuring start and stop are consistent.
#[test]
fn ssh_stop_plan_targets_pid_file() {
    let (_temp, paths) = temp_paths();

    let plan = build_ssh_stop_plan(&paths);

    assert_eq!(plan.program, "kill");
    assert_eq!(plan.pid_file, ssh_pid_file(&paths));
}
