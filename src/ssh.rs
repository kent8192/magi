//! SSH helpers for reaching a remote Redis instance through a local tunnel.
//!
//! When the Redis server that backs `magi`'s cross-agent messaging lives on a
//! remote host, this module manages an `ssh -L` port-forward so that the rest
//! of the CLI can keep talking to a `localhost` address as if Redis were local.
//! It is the implementation behind the `magi ssh` subcommands (`setup`/`start`,
//! `status`, and `stop`).
//!
//! Responsibilities:
//! - Build the exact `ssh` command line from the user's `AppConfig` (the
//!   `ssh.*` settings) without spawning a process, so the plan can be inspected
//!   and unit-tested in isolation (`build_ssh_start_plan`).
//! - Spawn and supervise the long-lived background `ssh` tunnel, recording its
//!   PID under the run directory so a later `stop` can find and terminate it
//!   (`start`).
//! - Report whether a tunnel is currently tracked (`status`).
//! - Safely tear the tunnel down, validating the recorded PID still belongs to
//!   an `ssh` process before sending a signal (`stop`).
//!
//! The tunnel's lifecycle is tracked entirely through a single PID file on disk
//! (see `ssh_pid_file`); there is no in-process handle that survives across
//! CLI invocations, which is why every operation re-reads configuration and the
//! PID file from `ConfigPaths`.

use std::fs;
use std::path::PathBuf;

use tokio::process::Command;

use crate::config::{AppConfig, ConfigPaths};
use crate::error::{MagiError, Result};

/// A fully-resolved description of how to launch the SSH tunnel.
///
/// Building this plan is separated from executing it so the command line can be
/// validated and unit-tested without actually spawning `ssh`. `start`
/// consumes a plan produced by `build_ssh_start_plan`.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SshStartPlan {
    /// The program to execute (always `"ssh"`).
    pub program: String,
    /// The argument vector passed to `ssh`, including `-N -L <forward>` and the
    /// destination host. See `build_ssh_start_plan` for the exact shape.
    pub args: Vec<String>,
    /// Path to the file where the spawned tunnel's PID is recorded so that a
    /// later `stop` can find and terminate it.
    pub pid_file: PathBuf,
}

/// A fully-resolved description of how to stop the SSH tunnel.
///
/// Produced by `build_ssh_stop_plan` and consumed by `stop`. The tunnel is
/// terminated by invoking `kill` with the PID read from `pid_file`.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SshStopPlan {
    /// The program used to terminate the tunnel (always `"kill"`).
    pub program: String,
    /// Path to the file holding the tunnel's recorded PID.
    pub pid_file: PathBuf,
}

/// Alias for `start`: brings up the SSH tunnel.
///
/// Exposed under a separate name so the CLI can offer both `magi ssh setup`
/// and `magi ssh start` for the same action.
///
/// # Errors
///
/// Propagates every error from `start`.
pub async fn setup() -> Result<()> {
    start().await
}

/// Spawns the background SSH tunnel and records its PID for later teardown.
///
/// Loads the active configuration, builds the launch plan, ensures the run
/// directory exists, spawns `ssh` detached (`-N`, no remote command), and
/// writes the child's PID to the plan's PID file. On success the tunnel keeps
/// running after this process exits.
///
/// # Errors
///
/// Returns an error if configuration cannot be loaded, the plan is invalid
/// (see `build_ssh_start_plan`), the run directory cannot be created, `ssh`
/// cannot be spawned, the spawned child has no PID, or the PID file cannot be
/// written. In the latter two cases the freshly spawned child is killed first
/// so no untracked tunnel is left running.
pub async fn start() -> Result<()> {
    // Re-derive on-disk locations and load `ssh.*` settings; nothing about a
    // previously started tunnel is kept in memory across CLI invocations.
    let paths = ConfigPaths::from_env()?;
    let config = AppConfig::load_from_paths(&paths)?;
    let plan = build_ssh_start_plan(&config, &paths)?;
    // The PID file lives under the run directory, which may not exist yet on a
    // first run; create the full parent chain before writing into it.
    if let Some(parent) = plan.pid_file.parent() {
        fs::create_dir_all(parent)?;
    }

    // Spawn `ssh` as a detached background process. Because of `-N` it sets up
    // the forward and then blocks, so the child stays alive as the tunnel.
    let mut child = Command::new(&plan.program).args(&plan.args).spawn()?;
    let pid = match child.id() {
        Some(pid) => pid,
        None => {
            // The tunnel exited before we could record it; reap it so it does
            // not linger as a zombie.
            let _ = child.kill().await;
            return Err(MagiError::CommandFailed(
                "failed to capture ssh pid".to_string(),
            ));
        }
    };

    // Persist the PID so a later `stop` can find this exact process. This is
    // the only handle on the tunnel once the current process exits.
    if let Err(error) = fs::write(&plan.pid_file, pid.to_string()) {
        // Do not leave an unmanaged tunnel running when its pid cannot be
        // persisted; without the pid file `stop` could never reach it.
        let _ = child.kill().await;
        return Err(error.into());
    }

    println!("ssh tunnel started pid={pid}");
    Ok(())
}

/// Reports whether an SSH tunnel is currently tracked by `magi`.
///
/// Reads the PID file and prints the recorded PID when present, or a
/// `stopped` message when the file is absent. This is a cheap, file-only
/// check: it reports what was recorded, not whether the process is actually
/// still alive (liveness is only verified during `stop`).
///
/// # Errors
///
/// Returns an error if configuration paths cannot be derived or if reading the
/// PID file fails for any reason other than the file not existing.
pub async fn status() -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    let pid_file = ssh_pid_file(&paths);
    // Treat a missing PID file as "not running" rather than an error; any other
    // I/O failure (permissions, etc.) is surfaced to the caller.
    match fs::read_to_string(&pid_file) {
        Ok(pid) => println!("ssh tunnel pid={}", pid.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("ssh tunnel stopped")
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

/// Tears down a previously started SSH tunnel, validating the PID first.
///
/// Reads the recorded PID, refuses to act on malformed or empty values, and
/// confirms the PID still names an `ssh` process before sending a signal. This
/// guards against a stale PID file whose number has since been recycled by an
/// unrelated process. On success the tunnel is killed and the PID file removed.
///
/// The function is idempotent in the common "already stopped" cases: a missing
/// PID file, an empty PID file, and a PID that no longer exists are all treated
/// as success (the stale file is cleaned up where applicable).
///
/// # Errors
///
/// Returns an error if configuration paths cannot be derived, the PID file
/// cannot be read (for reasons other than not existing) or parsed, the
/// recorded PID belongs to a non-`ssh` process (refusing to kill it), or the
/// `kill` command itself fails or exits non-zero.
pub async fn stop() -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    let plan = build_ssh_stop_plan(&paths);

    // No PID file means there is nothing to stop; this is a normal,
    // non-error outcome (e.g. `stop` run twice).
    let contents = match fs::read_to_string(&plan.pid_file) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("ssh tunnel is not running; nothing to stop");
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };

    // Validate the pid as a positive integer before it is ever passed to
    // `kill`; an unvalidated value such as `-1` would otherwise be interpreted
    // as a signal/target selector rather than a process id.
    let Some(pid) = crate::proc::parse_pid_file_contents(&contents)? else {
        println!(
            "ssh tunnel pid file at {} is empty; nothing to stop",
            plan.pid_file.display()
        );
        return Ok(());
    };

    // Confirm the recorded pid still belongs to an ssh process. A stale pid
    // file naming a reused pid must not be allowed to terminate an unrelated
    // process.
    match crate::proc::executable_name(pid).await? {
        None => {
            // The recorded process is gone (likely killed out-of-band); just
            // drop the orphaned PID file and report success.
            crate::proc::remove_file_if_exists(&plan.pid_file)?;
            println!(
                "ssh tunnel pid {pid} from {} is not running; removed stale pid file",
                plan.pid_file.display()
            );
            return Ok(());
        }
        // The PID was reused by some other program: abort rather than risk
        // killing an unrelated process.
        Some(executable) if !crate::proc::executable_basename_is(&executable, "ssh") => {
            return Err(MagiError::CommandFailed(format!(
                "pid {pid} from {} is `{executable}`, not ssh; refusing to kill",
                plan.pid_file.display()
            )));
        }
        // Confirmed live `ssh` process: fall through to actually kill it.
        Some(_) => {}
    }

    // Send the termination signal via the `kill` program described by the plan.
    let status = Command::new(&plan.program)
        .arg(pid.to_string())
        .status()
        .await?;
    if !status.success() {
        return Err(MagiError::CommandFailed(format!(
            "kill failed with status {status}"
        )));
    }
    // Only remove the PID file after the kill succeeded, so a failed stop
    // leaves the file intact and the tunnel still tracked.
    crate::proc::remove_file_if_exists(&plan.pid_file)?;
    println!("ssh tunnel stopped");
    Ok(())
}

/// Builds the `ssh` launch plan from the configured `ssh.*` settings.
///
/// Validates the configuration and assembles a local port-forward command of
/// the form `ssh -N -L <local_port>:<remote_host>:<remote_port> <host>`. The
/// `-N` flag means "do not run a remote command" so `ssh` only maintains the
/// forward, and `-L` establishes the local-to-remote forward that lets the
/// rest of `magi` reach Redis via `localhost:<local_port>`.
///
/// This function performs no I/O and spawns nothing; it only resolves the plan
/// so it can be inspected or unit-tested before `start` executes it.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` if `ssh.host` is empty, or if either
/// `ssh.local_port` or `ssh.remote_port` is `0` (ports must be in `1..=65535`).
pub fn build_ssh_start_plan(config: &AppConfig, paths: &ConfigPaths) -> Result<SshStartPlan> {
    // A destination host is mandatory; without it `ssh` has nowhere to connect.
    if config.ssh.host.trim().is_empty() {
        return Err(MagiError::InvalidConfig(
            "ssh.host is required for ssh tunnel".to_string(),
        ));
    }
    // Port 0 is never a valid forward endpoint; reject it up front so the error
    // is reported as a config problem rather than an opaque `ssh` failure.
    if config.ssh.local_port == 0 || config.ssh.remote_port == 0 {
        return Err(MagiError::InvalidConfig(
            "ssh ports must be between 1 and 65535".to_string(),
        ));
    }

    Ok(SshStartPlan {
        program: "ssh".to_string(),
        args: vec![
            // `-N`: establish the forward but do not execute a remote command.
            "-N".to_string(),
            // `-L`: define a local port-forward; the spec follows as the next arg.
            "-L".to_string(),
            // Forward spec: bind `local_port` locally and tunnel to
            // `remote_host:remote_port` on the far side of the SSH connection.
            format!(
                "{}:{}:{}",
                config.ssh.local_port, config.ssh.remote_host, config.ssh.remote_port
            ),
            // The SSH destination (user@host or an ssh-config alias).
            config.ssh.host.clone(),
        ],
        pid_file: ssh_pid_file(paths),
    })
}

/// Builds the plan used to stop the tunnel.
///
/// The stop plan simply pairs the `kill` program with the same PID file that
/// `start` writes, so `stop` can read the recorded PID and signal it.
pub fn build_ssh_stop_plan(paths: &ConfigPaths) -> SshStopPlan {
    SshStopPlan {
        program: "kill".to_string(),
        pid_file: ssh_pid_file(paths),
    }
}

/// Returns the canonical path of the SSH tunnel's PID file.
///
/// The file (`ssh-tunnel.pid`) lives under the run directory of
/// `ConfigPaths` (rooted at `~/.magi`). Centralizing the path here keeps
/// `start`, `status`, and `stop` in agreement about where the tunnel's
/// PID is recorded.
pub fn ssh_pid_file(paths: &ConfigPaths) -> PathBuf {
    paths.run_dir.join("ssh-tunnel.pid")
}
