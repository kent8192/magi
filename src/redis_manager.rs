use std::fs;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;

use rand::distr::Alphanumeric;
use rand::{rng, Rng};
use tokio::process::Command;

use crate::config::{AppConfig, ConfigPaths, RedisMode};
use crate::error::{MagiError, Result};
use crate::redis_client;

const CONTAINER_NAME: &str = "magi-redis";
const REDIS_IMAGE: &str = "redis:7-alpine";
const DOCKER_REDIS_CONFIG_MOUNT: &str = "/usr/local/etc/redis/redis.conf";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisServerStartPlan {
    pub program: String,
    pub args: Vec<String>,
    pub config_file: PathBuf,
    pub config_contents: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisServerStopPlan {
    pub program: String,
    pub pid_file: PathBuf,
}

pub trait RedisRuntime {
    fn start_docker<'a>(
        &'a mut self,
        paths: &'a ConfigPaths,
        bind: &'a str,
        port: u16,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

    fn start_redis_server<'a>(
        &'a mut self,
        paths: &'a ConfigPaths,
        bind: &'a str,
        port: u16,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

    fn ping<'a>(&'a mut self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

    fn run_command<'a>(
        &'a mut self,
        plan: &'a CommandPlan,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

    fn container_exists<'a>(
        &'a mut self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + 'a>>;

    fn process_executable<'a>(
        &'a mut self,
        pid: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + 'a>>;
}

#[derive(Debug, Default)]
struct RealRedisRuntime;

impl RedisRuntime for RealRedisRuntime {
    fn start_docker<'a>(
        &'a mut self,
        paths: &'a ConfigPaths,
        bind: &'a str,
        port: u16,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move { start_docker(paths, bind, port, password).await })
    }

    fn start_redis_server<'a>(
        &'a mut self,
        paths: &'a ConfigPaths,
        bind: &'a str,
        port: u16,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move { start_redis_server(paths, bind, port, password).await })
    }

    fn ping<'a>(&'a mut self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move { redis_client::ping(url).await })
    }

    fn run_command<'a>(
        &'a mut self,
        plan: &'a CommandPlan,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move { run_command(plan).await })
    }

    fn container_exists<'a>(
        &'a mut self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + 'a>> {
        Box::pin(async move { docker_container_exists(name).await })
    }

    fn process_executable<'a>(
        &'a mut self,
        pid: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + 'a>> {
        Box::pin(async move { crate::proc::executable_name(pid).await })
    }
}

pub async fn start(lan: bool, bind: Option<String>) -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    let config = AppConfig::load_from_paths(&paths)?;
    let mut runtime = RealRedisRuntime;

    let config = start_with_runtime(&paths, config, lan, bind, &mut runtime).await?;
    println!(
        "Redis started with {:?} on {}",
        config.redis.mode, config.redis.bind
    );
    Ok(())
}

pub async fn status() -> Result<()> {
    let config = AppConfig::load()?;
    let mut runtime = RealRedisRuntime;

    status_with_runtime(&config, &mut runtime).await?;
    println!(
        "Redis is reachable ({:?}, bind {}, port {})",
        config.redis.mode, config.redis.bind, config.redis.port
    );
    Ok(())
}

pub async fn stop() -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    let config = AppConfig::load_from_paths(&paths)?;
    let mut runtime = RealRedisRuntime;

    stop_with_runtime(&paths, &config, &mut runtime).await
}

pub async fn start_with_runtime(
    paths: &ConfigPaths,
    mut config: AppConfig,
    lan: bool,
    bind: Option<String>,
    runtime: &mut impl RedisRuntime,
) -> Result<AppConfig> {
    let bind = bind.unwrap_or_else(|| {
        if lan {
            "0.0.0.0".to_string()
        } else {
            "127.0.0.1".to_string()
        }
    });
    let port = config.redis.port;
    let password = password_for_start(config.redis.url.as_deref());
    let url = build_redis_url(&bind, port, &password)?;

    match runtime.start_docker(paths, &bind, port, &password).await {
        Ok(()) => {
            config.redis.mode = RedisMode::Docker;
            config.redis.bind = bind;
            config.redis.url = Some(url);
            config.save_to_paths(paths)?;
            Ok(config)
        }
        Err(docker_error) => {
            runtime
                .start_redis_server(paths, &bind, port, &password)
                .await
                .map_err(|redis_server_error| {
                    MagiError::CommandFailed(format!(
                        "Docker start failed ({docker_error}); redis-server fallback failed ({redis_server_error})"
                    ))
                })?;
            config.redis.mode = RedisMode::RedisServer;
            config.redis.bind = bind;
            config.redis.url = Some(url);
            config.save_to_paths(paths)?;
            Ok(config)
        }
    }
}

pub async fn status_with_runtime(
    config: &AppConfig,
    runtime: &mut impl RedisRuntime,
) -> Result<()> {
    let url = config
        .redis
        .url
        .as_deref()
        .ok_or_else(|| MagiError::InvalidConfig("redis.url is not configured".to_string()))?;

    runtime.ping(url).await?;
    Ok(())
}

pub async fn stop_with_runtime(
    paths: &ConfigPaths,
    config: &AppConfig,
    runtime: &mut impl RedisRuntime,
) -> Result<()> {
    match config.redis.mode {
        RedisMode::Docker => {
            // Treat a missing container as a no-op so `stop` stays idempotent,
            // mirroring the redis-server branch's "nothing to stop" handling.
            if !runtime.container_exists(CONTAINER_NAME).await? {
                println!("Docker Redis container {CONTAINER_NAME} not found; nothing to stop");
                return Ok(());
            }
            let plan = docker_stop_plan();
            runtime.run_command(&plan).await?;
            println!("Stopped Docker Redis container {CONTAINER_NAME}");
        }
        RedisMode::RedisServer => {
            let plan = redis_server_stop_plan(paths);
            if !plan.pid_file.exists() {
                println!(
                    "redis-server pid file not found at {}; nothing to stop",
                    plan.pid_file.display()
                );
                return Ok(());
            }

            // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path - pid file is fixed under HOME/.magi/run.
            let Some(pid) =
                crate::proc::parse_pid_file_contents(&fs::read_to_string(&plan.pid_file)?)?
            else {
                println!(
                    "redis-server pid file at {} is empty; nothing to stop",
                    plan.pid_file.display()
                );
                return Ok(());
            };

            // Verify the recorded pid still belongs to redis-server before
            // signalling it. A stale pid file could otherwise name a reused pid
            // and terminate an unrelated process.
            match runtime.process_executable(pid).await? {
                None => {
                    crate::proc::remove_file_if_exists(&plan.pid_file)?;
                    println!(
                        "redis-server pid {pid} from {} is not running; removed stale pid file",
                        plan.pid_file.display()
                    );
                    return Ok(());
                }
                Some(executable)
                    if !crate::proc::executable_basename_is(&executable, "redis-server") =>
                {
                    return Err(MagiError::CommandFailed(format!(
                        "pid {pid} from {} is `{executable}`, not redis-server; refusing to kill",
                        plan.pid_file.display()
                    )));
                }
                Some(_) => {}
            }

            runtime
                .run_command(&CommandPlan {
                    program: plan.program,
                    args: vec![pid.to_string()],
                })
                .await?;
            crate::proc::remove_file_if_exists(&plan.pid_file)?;
            println!("Stopped redis-server using {}", plan.pid_file.display());
        }
        RedisMode::External => {
            println!("Redis is configured as external; nothing to stop");
        }
    }

    Ok(())
}

pub fn build_docker_start_plan(
    paths: &ConfigPaths,
    bind: &str,
    port: u16,
    password: &str,
) -> Result<CommandPlan> {
    validate_managed_redis_auth(bind, password)?;

    Ok(CommandPlan {
        program: "docker".to_string(),
        args: vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            CONTAINER_NAME.to_string(),
            "-p".to_string(),
            format!("{bind}:{port}:6379"),
            "-v".to_string(),
            format!("{}:/data", paths.redis_data_dir.display()),
            "-v".to_string(),
            format!(
                "{}:{DOCKER_REDIS_CONFIG_MOUNT}:ro",
                docker_redis_config_file(paths).display()
            ),
            REDIS_IMAGE.to_string(),
            "redis-server".to_string(),
            DOCKER_REDIS_CONFIG_MOUNT.to_string(),
        ],
    })
}

pub fn build_redis_server_start_plan(
    paths: &ConfigPaths,
    bind: &str,
    port: u16,
    password: &str,
) -> Result<RedisServerStartPlan> {
    validate_managed_redis_auth(bind, password)?;

    let config_file = paths.redis_dir.join("redis.conf");
    let pid_file = redis_server_pid_file(paths);
    let config_contents = format!(
        concat!(
            "bind {bind}\n",
            "port {port}\n",
            "dir {dir}\n",
            "appendonly yes\n",
            "requirepass {password}\n",
            "daemonize yes\n",
            "pidfile {pid_file}\n"
        ),
        bind = bind,
        port = port,
        dir = paths.redis_data_dir.display(),
        password = password,
        pid_file = pid_file.display()
    );

    Ok(RedisServerStartPlan {
        program: "redis-server".to_string(),
        args: vec![config_file.display().to_string()],
        config_file,
        config_contents,
    })
}

pub fn docker_stop_plan() -> CommandPlan {
    CommandPlan {
        program: "docker".to_string(),
        args: vec![
            "rm".to_string(),
            "-f".to_string(),
            CONTAINER_NAME.to_string(),
        ],
    }
}

pub fn redis_server_stop_plan(paths: &ConfigPaths) -> RedisServerStopPlan {
    RedisServerStopPlan {
        program: "kill".to_string(),
        pid_file: redis_server_pid_file(paths),
    }
}

pub fn redis_server_pid_file(paths: &ConfigPaths) -> PathBuf {
    paths.run_dir.join("redis-server.pid")
}

pub fn docker_redis_config_file(paths: &ConfigPaths) -> PathBuf {
    paths.redis_dir.join("docker-redis.conf")
}

pub fn password_for_start(existing_url: Option<&str>) -> String {
    existing_url
        .and_then(extract_password_from_redis_url)
        .unwrap_or_else(generate_redis_password)
}

pub fn extract_password_from_redis_url(url: &str) -> Option<String> {
    let authority = url.strip_prefix("redis://")?.split('/').next()?;
    let credentials = authority.split('@').next()?;
    let password = credentials.strip_prefix(':')?;

    if password.is_empty() {
        None
    } else {
        Some(password.to_string())
    }
}

pub fn build_redis_url(bind: &str, port: u16, password: &str) -> Result<String> {
    validate_managed_redis_auth(bind, password)?;
    let host = if bind == "0.0.0.0" || bind == "::" {
        "127.0.0.1"
    } else {
        bind
    };

    Ok(format!("redis://:{password}@{host}:{port}"))
}

fn generate_redis_password() -> String {
    rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}

fn validate_managed_redis_auth(bind: &str, password: &str) -> Result<()> {
    if !is_local_bind(bind) && password.is_empty() {
        return Err(MagiError::InvalidConfig(
            "LAN Redis bind requires a non-empty password".to_string(),
        ));
    }
    Ok(())
}

fn is_local_bind(bind: &str) -> bool {
    matches!(bind, "127.0.0.1" | "localhost" | "::1")
}

pub fn redact_command_for_diagnostics(plan: &CommandPlan) -> String {
    let mut args = Vec::with_capacity(plan.args.len());
    let mut redact_next = false;

    for arg in &plan.args {
        if redact_next {
            args.push("<redacted>".to_string());
            redact_next = false;
            continue;
        }

        if is_sensitive_arg_name(arg) {
            args.push(arg.clone());
            redact_next = true;
            continue;
        }

        if let Some((name, _value)) = arg.split_once('=') {
            if is_sensitive_arg_name(name) {
                args.push(format!("{name}=<redacted>"));
                continue;
            }
        }

        args.push(arg.clone());
    }

    if args.is_empty() {
        plan.program.clone()
    } else {
        format!("{} {}", plan.program, args.join(" "))
    }
}

fn is_sensitive_arg_name(arg: &str) -> bool {
    let normalized = arg
        .trim_start_matches('-')
        .to_ascii_lowercase()
        .replace(['_', '-'], "");
    normalized.contains("password")
        || normalized.contains("pass")
        || normalized.contains("secret")
        || normalized.contains("token")
}

async fn start_docker(paths: &ConfigPaths, bind: &str, port: u16, password: &str) -> Result<()> {
    create_private_dir(&paths.redis_dir)?;
    create_private_dir(&paths.redis_data_dir)?;
    write_private_redis_config_file(
        &docker_redis_config_file(paths),
        docker_redis_config_contents(password).as_bytes(),
    )?;
    let _ = run_command_allow_failure(&docker_stop_plan()).await;
    let plan = build_docker_start_plan(paths, bind, port, password)?;
    run_command(&plan).await
}

fn docker_redis_config_contents(password: &str) -> String {
    format!(
        concat!(
            "bind 0.0.0.0\n",
            "port 6379\n",
            "dir /data\n",
            "appendonly yes\n",
            "requirepass {password}\n"
        ),
        password = password
    )
}

async fn start_redis_server(
    paths: &ConfigPaths,
    bind: &str,
    port: u16,
    password: &str,
) -> Result<()> {
    create_private_dir(&paths.redis_dir)?;
    create_private_dir(&paths.redis_data_dir)?;
    create_private_dir(&paths.run_dir)?;

    let plan = build_redis_server_start_plan(paths, bind, port, password)?;
    write_private_redis_config_file(&plan.config_file, plan.config_contents.as_bytes())?;
    run_command(&CommandPlan {
        program: plan.program,
        args: plan.args,
    })
    .await
}

async fn run_command(plan: &CommandPlan) -> Result<()> {
    let output = Command::new(&plan.program)
        .args(&plan.args)
        .stdin(Stdio::null())
        .output()
        .await?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let details = if stderr.is_empty() { stdout } else { stderr };
    Err(MagiError::CommandFailed(format!(
        "{} failed: {}",
        redact_command_for_diagnostics(plan),
        details
    )))
}

async fn run_command_allow_failure(plan: &CommandPlan) -> Result<()> {
    let _ = Command::new(&plan.program)
        .args(&plan.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    Ok(())
}

async fn docker_container_exists(name: &str) -> Result<bool> {
    // `docker container inspect` exits non-zero when the container is absent,
    // so its status lets us detect existence without parsing localized output.
    let status = Command::new("docker")
        .args(["container", "inspect", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await?;
    Ok(status.success())
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(path)?;
    // Keep managed Redis data and runtime files private on shared hosts.
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

#[cfg(unix)]
pub fn write_private_redis_config_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::ErrorKind;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let parent = path.parent().ok_or_else(|| {
        MagiError::InvalidConfig("Redis config path has no parent directory".to_string())
    })?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            MagiError::InvalidConfig("Redis config file name is not valid UTF-8".to_string())
        })?;

    for attempt in 0..100 {
        let temp_path = parent.join(format!(
            ".{file_name}.tmp.{}.{}",
            std::process::id(),
            attempt
        ));

        let mut temp_file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };

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

        return write_result;
    }

    Err(MagiError::InvalidConfig(
        "could not allocate private Redis config temp file".to_string(),
    ))
}

#[cfg(not(unix))]
pub fn write_private_redis_config_file(_path: &Path, _contents: &[u8]) -> Result<()> {
    Err(MagiError::InvalidConfig(
        "private Redis config permissions require Unix/macOS filesystem permissions".to_string(),
    ))
}
