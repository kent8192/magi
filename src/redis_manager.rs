//! Managed (embedded) Redis server lifecycle for the `magi` CLI.
//!
//! `magi` brokers cross-agent messaging through Redis (Streams + Pub/Sub). When
//! the operator has not pointed `magi` at an external Redis, this module starts,
//! inspects, and stops a Redis instance that `magi` owns ("managed Redis"). Two
//! managed backends are supported, attempted in order of preference:
//!
//! 1. A Docker container (`redis:7-alpine`) named `magi-redis`.
//! 2. A locally installed `redis-server` daemon, used as a fallback when Docker
//!    is unavailable or fails to start.
//!
//! The public `start` / `status` / `stop` entry points are thin wrappers around
//! their `*_with_runtime` counterparts. The split exists so tests can inject a
//! fake `RedisRuntime` and exercise the orchestration logic without touching
//! Docker, spawning processes, or talking to a real Redis. The chosen backend,
//! bind address, and generated connection URL are persisted into the `magi`
//! `AppConfig` so later commands can reach the same instance.
//!
//! Security and state-layout notes that this module is responsible for:
//! - Managed Redis data, runtime, and config files live under `~/.magi` and are
//!   created with `0o700` directories / `0o600` files on Unix.
//! - A randomly generated password is required whenever Redis binds to a
//!   non-loopback address (LAN mode); see `validate_managed_redis_auth`.
//! - Diagnostic output redacts password-like arguments via
//!   `redact_command_for_diagnostics`.
//!
//! See also `crate::config` for the on-disk paths and `RedisMode`, and
//! `crate::redis_client` for the connection/ping layer.

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

/// Fixed name of the Docker container that hosts managed Redis. Used both when
/// creating the container and when locating it for `inspect` / `rm`.
const CONTAINER_NAME: &str = "magi-redis";
/// Pinned Redis image for the Docker backend (Alpine variant for a small footprint).
const REDIS_IMAGE: &str = "redis:7-alpine";
/// In-container path where the generated `redis.conf` is bind-mounted read-only,
/// and the argument passed to `redis-server` inside the container.
const DOCKER_REDIS_CONFIG_MOUNT: &str = "/usr/local/etc/redis/redis.conf";

/// A fully resolved external command to execute (program plus arguments).
///
/// Building the command as a plain data value (rather than spawning eagerly)
/// makes the orchestration logic deterministic and unit-testable: tests can
/// assert on the exact program/args without running anything, and the real
/// runtime simply hands the plan to `run_command`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    /// Executable to invoke (e.g. `docker`, `redis-server`, `kill`).
    pub program: String,
    /// Ordered argument list passed verbatim to `program`.
    pub args: Vec<String>,
}

/// Everything required to launch the `redis-server` fallback backend.
///
/// Besides the command itself, this carries the config file path and its
/// rendered contents so the caller can write the file privately (`0o600`)
/// before spawning the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisServerStartPlan {
    /// Executable to launch (`redis-server`).
    pub program: String,
    /// Arguments — typically a single path to the generated config file.
    pub args: Vec<String>,
    /// Path the rendered `redis.conf` must be written to before launch.
    pub config_file: PathBuf,
    /// Rendered `redis.conf` body (bind/port/dir/auth/daemonize/pidfile).
    pub config_contents: String,
}

/// Inputs needed to stop the `redis-server` fallback backend.
///
/// The stop flow reads the recorded pid from `pid_file`, validates that it
/// still belongs to `redis-server`, then signals it with `program` (`kill`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisServerStopPlan {
    /// Signal-sending program (`kill`).
    pub program: String,
    /// Path to the pid file written by the daemon under `~/.magi/run`.
    pub pid_file: PathBuf,
}

/// Seam between the lifecycle orchestration and the real world.
///
/// Every operation that touches Docker, spawns a process, reads `/proc`, or
/// connects to Redis is funneled through this trait so the higher-level
/// `*_with_runtime` functions can be driven by a test double. Methods return
/// boxed, lifetime-bound futures (`Pin<Box<dyn Future + 'a>>`) rather than using
/// `async fn`, keeping the trait object-safe and the borrowed arguments alive
/// for the duration of the returned future.
pub trait RedisRuntime {
    /// Start the Docker-backed managed Redis container.
    ///
    /// `bind`/`port`/`password` configure the container's published address and
    /// auth. Returns an error if Docker is unavailable or the run fails; the
    /// caller then falls back to `RedisRuntime::start_redis_server`.
    fn start_docker<'a>(
        &'a mut self,
        paths: &'a ConfigPaths,
        bind: &'a str,
        port: u16,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

    /// Start the local `redis-server` fallback daemon.
    ///
    /// Used when the Docker backend cannot be started. Writes a private config
    /// file and launches `redis-server` in daemonized mode.
    fn start_redis_server<'a>(
        &'a mut self,
        paths: &'a ConfigPaths,
        bind: &'a str,
        port: u16,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

    /// Verify reachability of a Redis instance at `url` (a `PING` round-trip).
    fn ping<'a>(&'a mut self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

    /// Execute a resolved `CommandPlan`, failing if the command exits non-zero.
    fn run_command<'a>(
        &'a mut self,
        plan: &'a CommandPlan,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

    /// Report whether a Docker container named `name` currently exists.
    fn container_exists<'a>(
        &'a mut self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + 'a>>;

    /// Resolve the executable name backing process `pid`, if it is still running.
    ///
    /// Returns `Ok(None)` when no such process exists. Used to confirm a pid
    /// file still names a live `redis-server` before signalling it, guarding
    /// against killing an unrelated process that reused the pid.
    fn process_executable<'a>(
        &'a mut self,
        pid: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + 'a>>;
}

/// Production `RedisRuntime` that drives real Docker, processes, and Redis.
///
/// Each method simply delegates to the corresponding free function / module in
/// this crate; the type holds no state and is constructed at the public entry
/// points.
#[derive(Debug, Default)]
struct RealRedisRuntime;

/// Real implementation: every method forwards to the concrete free function
/// that performs the actual side effect.
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

/// CLI entry point for `magi redis start`.
///
/// Loads on-disk config, starts managed Redis (Docker first, `redis-server`
/// fallback), persists the resulting mode/bind/URL, and prints a summary.
///
/// `lan` selects a `0.0.0.0` bind when `true` (loopback otherwise); `bind`
/// overrides that choice with an explicit address.
///
/// # Errors
/// Propagates config-load/save errors and any failure from
/// `start_with_runtime` (e.g. both backends failing to start).
pub async fn start(lan: bool, bind: Option<String>) -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    let config = AppConfig::load_from_paths(&paths)?;
    let mut runtime = RealRedisRuntime;

    // Delegate to the testable core, then report the backend that was selected.
    let config = start_with_runtime(&paths, config, lan, bind, &mut runtime).await?;
    println!(
        "Redis started with {:?} on {}",
        config.redis.mode, config.redis.bind
    );
    Ok(())
}

/// CLI entry point for `magi redis status`.
///
/// Pings the configured Redis URL and prints the mode/bind/port on success.
///
/// # Errors
/// Returns an error if config cannot be loaded, `redis.url` is unset, or the
/// instance is unreachable.
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

/// CLI entry point for `magi redis stop`.
///
/// Tears down whichever managed backend is recorded in config. Idempotent: a
/// missing container / pid file is reported and treated as success.
///
/// # Errors
/// Propagates config-load errors and any failure from `stop_with_runtime`.
pub async fn stop() -> Result<()> {
    let paths = ConfigPaths::from_env()?;
    let config = AppConfig::load_from_paths(&paths)?;
    let mut runtime = RealRedisRuntime;

    stop_with_runtime(&paths, &config, &mut runtime).await
}

/// Runtime-injectable core of `start`: choose a bind address, derive auth,
/// start managed Redis, and persist the outcome into `config`.
///
/// Backend selection is "Docker first, `redis-server` fallback". On success the
/// updated `AppConfig` (with `mode`, `bind`, and `url` set) is saved to disk
/// and returned so callers can report it.
///
/// Parameters:
/// - `lan`: when `true` and no explicit `bind` is given, bind to `0.0.0.0`.
/// - `bind`: explicit override of the bind address.
/// - `runtime`: the `RedisRuntime` seam (real or test double).
///
/// # Errors
/// Returns `MagiError::CommandFailed` if both Docker and the `redis-server`
/// fallback fail to start (the message includes both underlying errors), and
/// propagates URL-build / config-save errors.
pub async fn start_with_runtime(
    paths: &ConfigPaths,
    mut config: AppConfig,
    lan: bool,
    bind: Option<String>,
    runtime: &mut impl RedisRuntime,
) -> Result<AppConfig> {
    // Default bind: loopback for local-only, 0.0.0.0 for LAN exposure.
    let bind = bind.unwrap_or_else(|| {
        if lan {
            "0.0.0.0".to_string()
        } else {
            "127.0.0.1".to_string()
        }
    });
    let port = config.redis.port;
    // Reuse the password embedded in any existing URL, else generate a fresh one,
    // so restarting managed Redis keeps the same credentials.
    let password = password_for_start(config.redis.url.as_deref());
    let url = build_redis_url(&bind, port, &password)?;

    // Prefer the Docker backend; fall back to a local redis-server daemon.
    match runtime.start_docker(paths, &bind, port, &password).await {
        Ok(()) => {
            config.redis.mode = RedisMode::Docker;
            config.redis.bind = bind;
            config.redis.url = Some(url);
            config.save_to_paths(paths)?;
            Ok(config)
        }
        Err(docker_error) => {
            // Both failures are surfaced together so the operator can see why
            // each backend was rejected.
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

/// Runtime-injectable core of `status`: ping the configured Redis URL.
///
/// # Errors
/// Returns `MagiError::InvalidConfig` when `redis.url` is unset, and
/// propagates any connectivity error from `RedisRuntime::ping`.
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

/// Runtime-injectable core of `stop`: tear down the recorded managed backend.
///
/// Behavior per `RedisMode`:
/// - `Docker`: remove the `magi-redis` container (no-op if absent).
/// - `RedisServer`: read the pid file, verify the pid still names a live
///   `redis-server`, then signal it and clean up the pid file.
/// - `External`: nothing to stop (Redis is operator-managed).
///
/// The function is idempotent — missing containers, missing/empty pid files,
/// and already-dead processes are reported and treated as success.
///
/// # Errors
/// Returns `MagiError::CommandFailed` if the recorded pid belongs to a
/// non-`redis-server` process (refusing to kill it), and propagates command /
/// filesystem errors.
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
            // `docker rm -f` both stops and removes the container in one step.
            let plan = docker_stop_plan();
            runtime.run_command(&plan).await?;
            println!("Stopped Docker Redis container {CONTAINER_NAME}");
        }
        RedisMode::RedisServer => {
            // The daemon records its pid under ~/.magi/run; absence means there
            // is nothing to stop.
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

            // Validation passed: signal the verified redis-server pid, then
            // remove the now-stale pid file.
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
            // magi does not own an external Redis, so it must not stop it.
            println!("Redis is configured as external; nothing to stop");
        }
    }

    Ok(())
}

/// Build the `docker run` plan for the Docker-backed managed Redis container.
///
/// Publishes `bind:port` to the container's `6379`, bind-mounts the data
/// directory at `/data`, and mounts the generated `redis.conf` read-only at
/// `DOCKER_REDIS_CONFIG_MOUNT`, which `redis-server` then loads.
///
/// # Errors
/// Returns `MagiError::InvalidConfig` via `validate_managed_redis_auth` when
/// a LAN bind is requested without a password.
pub fn build_docker_start_plan(
    paths: &ConfigPaths,
    bind: &str,
    port: u16,
    password: &str,
) -> Result<CommandPlan> {
    // Refuse to expose Redis on the network without authentication.
    validate_managed_redis_auth(bind, password)?;

    Ok(CommandPlan {
        program: "docker".to_string(),
        args: vec![
            "run".to_string(),
            "-d".to_string(),
            "--name".to_string(),
            CONTAINER_NAME.to_string(),
            // Publish host bind:port to the container's standard 6379.
            "-p".to_string(),
            format!("{bind}:{port}:6379"),
            // Persist append-only data on the host.
            "-v".to_string(),
            format!("{}:/data", paths.redis_data_dir.display()),
            // Mount the host-rendered config read-only and load it explicitly.
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

/// Build the launch plan for the local `redis-server` fallback daemon.
///
/// Renders a `redis.conf` (bind/port/data-dir/append-only/auth, daemonized,
/// with a pid file) and points `redis-server` at it. The caller is responsible
/// for writing `config_contents` to `config_file` privately before launching.
///
/// # Errors
/// Returns `MagiError::InvalidConfig` via `validate_managed_redis_auth` when
/// a LAN bind is requested without a password.
pub fn build_redis_server_start_plan(
    paths: &ConfigPaths,
    bind: &str,
    port: u16,
    password: &str,
) -> Result<RedisServerStartPlan> {
    validate_managed_redis_auth(bind, password)?;

    // Config and pid files live under the per-user ~/.magi tree.
    let config_file = paths.redis_dir.join("redis.conf");
    let pid_file = redis_server_pid_file(paths);
    // Render redis.conf; `daemonize yes` detaches the server and writes pidfile.
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

/// Build the `docker rm -f magi-redis` plan that stops and removes the container.
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

/// Build the plan for stopping the `redis-server` fallback: `kill` plus the pid
/// file to read the target process from.
pub fn redis_server_stop_plan(paths: &ConfigPaths) -> RedisServerStopPlan {
    RedisServerStopPlan {
        program: "kill".to_string(),
        pid_file: redis_server_pid_file(paths),
    }
}

/// Path of the `redis-server` pid file (`~/.magi/run/redis-server.pid`).
pub fn redis_server_pid_file(paths: &ConfigPaths) -> PathBuf {
    paths.run_dir.join("redis-server.pid")
}

/// Path of the host-side Docker `redis.conf`
/// (`~/.magi/redis/docker-redis.conf`), bind-mounted into the container.
pub fn docker_redis_config_file(paths: &ConfigPaths) -> PathBuf {
    paths.redis_dir.join("docker-redis.conf")
}

/// Determine the password to use when (re)starting managed Redis.
///
/// Reuses the password embedded in `existing_url` when present so restarts keep
/// stable credentials; otherwise generates a fresh random password.
pub fn password_for_start(existing_url: Option<&str>) -> String {
    existing_url
        .and_then(extract_password_from_redis_url)
        .unwrap_or_else(generate_redis_password)
}

/// Extract the password from a `redis://:<password>@host:port` URL.
///
/// Parses the authority (`:password@host`) out of the URL and returns the
/// password component. Returns `None` if the URL has no `redis://` prefix, no
/// `:password@` credentials, or an empty password — the inverse of the format
/// produced by `build_redis_url`.
pub fn extract_password_from_redis_url(url: &str) -> Option<String> {
    // Strip the scheme, then take the authority up to the first path segment.
    let authority = url.strip_prefix("redis://")?.split('/').next()?;
    // The credentials are everything before the `@host` separator.
    let credentials = authority.split('@').next()?;
    // Managed URLs use an empty username, so the password follows the leading `:`.
    let password = credentials.strip_prefix(':')?;

    if password.is_empty() {
        None
    } else {
        Some(password.to_string())
    }
}

/// Build the `redis://:<password>@host:port` connection URL for managed Redis.
///
/// When Redis binds to a wildcard address (`0.0.0.0` / `::`), the client URL
/// still connects over loopback, so the host is rewritten to `127.0.0.1`.
///
/// # Errors
/// Returns `MagiError::InvalidConfig` via `validate_managed_redis_auth` when
/// a LAN bind is requested without a password.
pub fn build_redis_url(bind: &str, port: u16, password: &str) -> Result<String> {
    validate_managed_redis_auth(bind, password)?;
    // A wildcard bind is not a connectable host; clients reach it via loopback.
    let host = if bind == "0.0.0.0" || bind == "::" {
        "127.0.0.1"
    } else {
        bind
    };

    Ok(format!("redis://:{password}@{host}:{port}"))
}

/// Generate a 48-character alphanumeric password for managed Redis auth.
fn generate_redis_password() -> String {
    rng()
        .sample_iter(&Alphanumeric)
        .take(48)
        .map(char::from)
        .collect()
}

/// Enforce that network-exposed managed Redis always has a password.
///
/// # Errors
/// Returns `MagiError::InvalidConfig` when `bind` is non-loopback and
/// `password` is empty.
fn validate_managed_redis_auth(bind: &str, password: &str) -> Result<()> {
    if !is_local_bind(bind) && password.is_empty() {
        return Err(MagiError::InvalidConfig(
            "LAN Redis bind requires a non-empty password".to_string(),
        ));
    }
    Ok(())
}

/// Whether `bind` is a loopback-only address that is safe without a password.
fn is_local_bind(bind: &str) -> bool {
    matches!(bind, "127.0.0.1" | "localhost" | "::1")
}

/// Render a `CommandPlan` into a single diagnostic string with secrets redacted.
///
/// Two redaction shapes are handled so passwords never leak into error
/// messages or logs:
/// - Separate-argument form: a sensitive flag name (e.g. `--requirepass`)
///   redacts the argument that follows it.
/// - Inline `name=value` form: a sensitive `name` redacts only the value.
///
/// Returns the program alone when there are no arguments.
pub fn redact_command_for_diagnostics(plan: &CommandPlan) -> String {
    let mut args = Vec::with_capacity(plan.args.len());
    // When set, the previous arg was a sensitive flag whose value comes next.
    let mut redact_next = false;

    for arg in &plan.args {
        if redact_next {
            // This positional value belongs to a sensitive flag seen previously.
            args.push("<redacted>".to_string());
            redact_next = false;
            continue;
        }

        if is_sensitive_arg_name(arg) {
            // Keep the flag name visible but redact the value in the next arg.
            args.push(arg.clone());
            redact_next = true;
            continue;
        }

        if let Some((name, _value)) = arg.split_once('=') {
            // Inline `name=value`: redact only the value portion.
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

/// Heuristic: does `arg` name a secret-bearing flag (password/secret/token)?
///
/// Normalizes by stripping leading dashes, lowercasing, and removing `_`/`-`
/// so variants like `--requirepass`, `REDIS_PASSWORD`, and `auth-token` all match.
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

/// Start the Docker-backed managed Redis container end to end.
///
/// Prepares the private state directories, writes the container's `redis.conf`,
/// removes any leftover `magi-redis` container, then runs the start plan.
///
/// # Errors
/// Propagates directory/config creation errors and any non-zero exit from the
/// `docker run` invocation.
async fn start_docker(paths: &ConfigPaths, bind: &str, port: u16, password: &str) -> Result<()> {
    // Ensure ~/.magi/redis and the data dir exist with private (0o700) perms.
    create_private_dir(&paths.redis_dir)?;
    create_private_dir(&paths.redis_data_dir)?;
    // Render the container's redis.conf privately before it is bind-mounted.
    write_private_redis_config_file(
        &docker_redis_config_file(paths),
        docker_redis_config_contents(password).as_bytes(),
    )?;
    // Best-effort removal of a stale container so `docker run --name` won't clash;
    // failure here is expected when no prior container exists, hence ignored.
    let _ = run_command_allow_failure(&docker_stop_plan()).await;
    let plan = build_docker_start_plan(paths, bind, port, password)?;
    run_command(&plan).await
}

/// Render the `redis.conf` used inside the Docker container.
///
/// The container always binds `0.0.0.0:6379` internally; host-side exposure is
/// controlled by the `docker run -p` publish mapping, not this file.
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

/// Start the `redis-server` fallback daemon end to end.
///
/// Creates the private state directories (including the run dir for the pid
/// file), writes the rendered `redis.conf`, then launches the daemonized server.
///
/// # Errors
/// Propagates directory/config creation errors and any non-zero exit from
/// `redis-server`.
async fn start_redis_server(
    paths: &ConfigPaths,
    bind: &str,
    port: u16,
    password: &str,
) -> Result<()> {
    // run_dir is also required here because the daemon writes its pid file there.
    create_private_dir(&paths.redis_dir)?;
    create_private_dir(&paths.redis_data_dir)?;
    create_private_dir(&paths.run_dir)?;

    let plan = build_redis_server_start_plan(paths, bind, port, password)?;
    // Write the config privately, then hand the file path to redis-server.
    write_private_redis_config_file(&plan.config_file, plan.config_contents.as_bytes())?;
    run_command(&CommandPlan {
        program: plan.program,
        args: plan.args,
    })
    .await
}

/// Run a `CommandPlan` to completion, treating a non-zero exit as an error.
///
/// `stdin` is closed so spawned tools never block waiting for input. On failure,
/// the redacted command and trimmed stderr (falling back to stdout) are folded
/// into a `MagiError::CommandFailed`.
///
/// # Errors
/// Returns an error if the process cannot be spawned or exits unsuccessfully.
async fn run_command(plan: &CommandPlan) -> Result<()> {
    let output = Command::new(&plan.program)
        .args(&plan.args)
        .stdin(Stdio::null())
        .output()
        .await?;

    if output.status.success() {
        return Ok(());
    }

    // Prefer stderr for the failure detail, but fall back to stdout if empty.
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let details = if stderr.is_empty() { stdout } else { stderr };
    Err(MagiError::CommandFailed(format!(
        "{} failed: {}",
        // Redact secrets so passwords never reach error output.
        redact_command_for_diagnostics(plan),
        details
    )))
}

/// Run a `CommandPlan` for side effects only, ignoring its exit status.
///
/// All standard streams are silenced. Used for best-effort cleanup (e.g.
/// removing a leftover container) where a non-zero exit is an acceptable outcome.
///
/// # Errors
/// Returns an error only if the process cannot be spawned at all.
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

/// Detect whether a Docker container named `name` exists.
///
/// # Errors
/// Returns an error if the `docker` process cannot be spawned.
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

/// Create `path` (and parents) with owner-only `0o700` permissions on Unix.
///
/// Keeps managed Redis state private on shared/multi-user hosts.
///
/// # Errors
/// Propagates directory-creation and permission-setting failures.
#[cfg(unix)]
fn create_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(path)?;
    // Keep managed Redis data and runtime files private on shared hosts.
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Non-Unix fallback: create the directory without Unix permission bits.
///
/// # Errors
/// Propagates directory-creation failures.
#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    Ok(())
}

/// Atomically write `contents` to `path` with owner-only `0o600` permissions.
///
/// The write is crash-safe and never exposes a partially written or
/// world-readable config: contents are written to a uniquely named temp file in
/// the same directory (created with `O_CREAT|O_EXCL` and mode `0o600`), `fsync`ed,
/// then `rename`d over `path` (an atomic replace on the same filesystem). On any
/// failure the temp file is removed.
///
/// The temp file name embeds the process id and an attempt counter; the loop
/// retries when a candidate name already exists, tolerating concurrent writers
/// and stale temp files.
///
/// # Errors
/// Returns `MagiError::InvalidConfig` when `path` has no parent or a
/// non-UTF-8 file name, or when no temp name could be allocated after 100
/// attempts; propagates I/O errors from create/write/sync/rename.
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

    // Bounded retry loop: find a temp name that does not already exist.
    for attempt in 0..100 {
        // Temp lives beside the target so the later rename stays on one filesystem.
        let temp_path = parent.join(format!(
            ".{file_name}.tmp.{}.{}",
            std::process::id(),
            attempt
        ));

        // O_CREAT|O_EXCL with mode 0o600: fails if the name is taken, and never
        // briefly exposes a world-readable file.
        let mut temp_file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)
        {
            Ok(file) => file,
            // Name collision: try the next attempt index.
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };

        // Write + fsync + atomic rename, re-asserting 0o600 before and after.
        let write_result = (|| -> Result<()> {
            temp_file.write_all(contents)?;
            // fsync the data before the rename so a crash cannot leave it empty.
            temp_file.sync_all()?;
            drop(temp_file);
            fs::set_permissions(&temp_path, fs::Permissions::from_mode(0o600))?;
            // Atomic replace: readers see either the old or the new file, never partial.
            fs::rename(&temp_path, path)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            Ok(())
        })();

        // On failure, do not leave the temp file lying around.
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }

        return write_result;
    }

    Err(MagiError::InvalidConfig(
        "could not allocate private Redis config temp file".to_string(),
    ))
}

/// Non-Unix stub: managed Redis config privacy relies on Unix permission bits.
///
/// # Errors
/// Always returns `MagiError::InvalidConfig`; managed Redis is Unix/macOS-only.
#[cfg(not(unix))]
pub fn write_private_redis_config_file(_path: &Path, _contents: &[u8]) -> Result<()> {
    Err(MagiError::InvalidConfig(
        "private Redis config permissions require Unix/macOS filesystem permissions".to_string(),
    ))
}
