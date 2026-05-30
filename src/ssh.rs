use std::fs;
use std::path::PathBuf;

use tokio::process::Command;

use crate::config::{AppConfig, ConfigPaths};
use crate::error::{MagiError, Result};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SshStartPlan {
    pub program: String,
    pub args: Vec<String>,
    pub pid_file: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct SshStopPlan {
    pub program: String,
    pub pid_file: PathBuf,
}

pub async fn setup() -> Result<()> {
    start().await
}

pub async fn start() -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    let config = AppConfig::load_from_paths(&paths)?;
    let plan = build_ssh_start_plan(&config, &paths)?;
    if let Some(parent) = plan.pid_file.parent() {
        fs::create_dir_all(parent)?;
    }

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

    if let Err(error) = fs::write(&plan.pid_file, pid.to_string()) {
        // Do not leave an unmanaged tunnel running when its pid cannot be
        // persisted; without the pid file `stop` could never reach it.
        let _ = child.kill().await;
        return Err(error.into());
    }

    println!("ssh tunnel started pid={pid}");
    Ok(())
}

pub async fn status() -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    let pid_file = ssh_pid_file(&paths);
    match fs::read_to_string(&pid_file) {
        Ok(pid) => println!("ssh tunnel pid={}", pid.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!("ssh tunnel stopped")
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub async fn stop() -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    let plan = build_ssh_stop_plan(&paths);

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
            crate::proc::remove_file_if_exists(&plan.pid_file)?;
            println!(
                "ssh tunnel pid {pid} from {} is not running; removed stale pid file",
                plan.pid_file.display()
            );
            return Ok(());
        }
        Some(executable) if !crate::proc::executable_basename_is(&executable, "ssh") => {
            return Err(MagiError::CommandFailed(format!(
                "pid {pid} from {} is `{executable}`, not ssh; refusing to kill",
                plan.pid_file.display()
            )));
        }
        Some(_) => {}
    }

    let status = Command::new(&plan.program)
        .arg(pid.to_string())
        .status()
        .await?;
    if !status.success() {
        return Err(MagiError::CommandFailed(format!(
            "kill failed with status {status}"
        )));
    }
    crate::proc::remove_file_if_exists(&plan.pid_file)?;
    println!("ssh tunnel stopped");
    Ok(())
}

pub fn build_ssh_start_plan(config: &AppConfig, paths: &ConfigPaths) -> Result<SshStartPlan> {
    if config.ssh.host.trim().is_empty() {
        return Err(MagiError::InvalidConfig(
            "ssh.host is required for ssh tunnel".to_string(),
        ));
    }
    if config.ssh.local_port == 0 || config.ssh.remote_port == 0 {
        return Err(MagiError::InvalidConfig(
            "ssh ports must be between 1 and 65535".to_string(),
        ));
    }

    Ok(SshStartPlan {
        program: "ssh".to_string(),
        args: vec![
            "-N".to_string(),
            "-L".to_string(),
            format!(
                "{}:{}:{}",
                config.ssh.local_port, config.ssh.remote_host, config.ssh.remote_port
            ),
            config.ssh.host.clone(),
        ],
        pid_file: ssh_pid_file(paths),
    })
}

pub fn build_ssh_stop_plan(paths: &ConfigPaths) -> SshStopPlan {
    SshStopPlan {
        program: "kill".to_string(),
        pid_file: ssh_pid_file(paths),
    }
}

pub fn ssh_pid_file(paths: &ConfigPaths) -> PathBuf {
    paths.run_dir.join("ssh-tunnel.pid")
}
