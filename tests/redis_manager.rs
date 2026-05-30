//! Tests for the managed Redis lifecycle module (`redis_manager`).
//!
//! This suite verifies every major responsibility of `redis_manager` without
//! requiring a live Docker daemon or a real `redis-server` binary.  All
//! platform I/O is driven through the [`RedisRuntime`] trait; the concrete
//! implementation used here is [`FakeRuntime`], which records every call and
//! returns pre-configured results.
//!
//! # Test-gate
//!
//! Tests that actually connect to a Redis instance are guarded by the
//! `MAGI_REQUIRE_REDIS_TESTS` environment variable (not present in this file —
//! those live in a separate integration suite).  Everything here is fully
//! deterministic and runs in any environment.
//!
//! # Coverage areas
//!
//! - Command-plan construction for Docker and `redis-server` launch/stop
//! - Password security: LAN bind without password is rejected, passwords are
//!   never embedded in plain-text arguments, and existing URL passwords are
//!   reused rather than regenerated
//! - `start_with_runtime`: Docker-first launch, fallback to `redis-server`,
//!   and error propagation when both backends fail; persisted config is verified
//!   by re-loading it after each successful start
//! - `status_with_runtime`: URL is forwarded to `ping`; missing URL yields a
//!   clear `InvalidConfig` error
//! - `stop_with_runtime`: Docker container removal, `redis-server` PID-file
//!   lifecycle (valid PID, stale/gone process, foreign process, empty/whitespace/
//!   negative/non-numeric PID file contents), and the no-op for external mode
//! - `write_private_redis_config_file`: symlink replacement and `0o600` mode
//!   enforcement (Unix only)
//! - `redact_command_for_diagnostics`: sensitive argument values are hidden

use std::future::Future;
use std::pin::Pin;

use magi::config::{AppConfig, ConfigPaths, RedisMode};
use magi::error::{MagiError, Result};
use magi::redis_manager::{
    build_docker_start_plan, build_redis_server_start_plan, build_redis_url, docker_stop_plan,
    extract_password_from_redis_url, password_for_start, redact_command_for_diagnostics,
    redis_server_pid_file, redis_server_stop_plan, start_with_runtime, status_with_runtime,
    stop_with_runtime, write_private_redis_config_file, CommandPlan, RedisRuntime,
};

/// In-memory stand-in for the real [`RedisRuntime`] used during tests.
///
/// Each method records its call arguments in a `*_calls` / `*_starts` /
/// `pings` / `commands` field so that assertions can inspect what the
/// production code requested.  The outcome of the *first* call is taken from
/// the corresponding `*_result` field (via `Option::take`); subsequent calls
/// default to `Ok(())` / the "process is `redis-server`" sentinel.
#[derive(Debug, Default)]
struct FakeRuntime {
    /// Result returned by the next `start_docker` call (`None` → `Ok(())`).
    docker_result: Option<Result<()>>,
    /// Result returned by the next `start_redis_server` call (`None` → `Ok(())`).
    redis_server_result: Option<Result<()>>,
    /// Result returned by the next `ping` call (`None` → `Ok(())`).
    ping_result: Option<Result<()>>,
    /// Ordered results consumed by successive `run_command` calls (empty → `Ok(())`).
    command_results: Vec<Result<()>>,
    /// Result returned by the next `container_exists` call (`None` → `Ok(true)`).
    container_exists_result: Option<Result<bool>>,
    /// Result returned by the next `process_executable` call (`None` → `Ok(Some("redis-server"))`).
    process_executable_result: Option<Result<Option<String>>>,
    /// Recorded argument sets passed to each `start_docker` invocation.
    docker_starts: Vec<CommandPlan>,
    /// Recorded argument sets passed to each `start_redis_server` invocation.
    redis_server_starts: Vec<CommandPlan>,
    /// Redis URLs passed to each `ping` invocation.
    pings: Vec<String>,
    /// [`CommandPlan`]s forwarded to each `run_command` invocation.
    commands: Vec<CommandPlan>,
    /// Container names passed to each `container_exists` invocation.
    container_exists_calls: Vec<String>,
    /// PIDs passed to each `process_executable` invocation.
    process_executable_calls: Vec<u32>,
}

impl FakeRuntime {
    /// Convenience constructor for a successful result, used as the default
    /// fallback when no pre-configured result is present.
    fn ok() -> Result<()> {
        Ok(())
    }

    /// Convenience constructor for a `CommandFailed` error with the given
    /// `message`, used to inject failures in specific test scenarios.
    fn fail(message: &str) -> Result<()> {
        Err(MagiError::CommandFailed(message.to_string()))
    }
}

/// `FakeRuntime` fulfils all [`RedisRuntime`] methods by recording calls and
/// returning pre-configured results, so the test suite never touches real
/// processes or the network.
impl RedisRuntime for FakeRuntime {
    fn start_docker<'a>(
        &'a mut self,
        paths: &'a ConfigPaths,
        bind: &'a str,
        port: u16,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            // Build the real plan so the argument structure is verified, then
            // record it; the actual `docker run` is never executed.
            self.docker_starts
                .push(build_docker_start_plan(paths, bind, port, password)?);
            self.docker_result.take().unwrap_or_else(Self::ok)
        })
    }

    fn start_redis_server<'a>(
        &'a mut self,
        paths: &'a ConfigPaths,
        bind: &'a str,
        port: u16,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            // Build the real plan (validates configuration) and record program +
            // args; the config_file and config_contents fields are intentionally
            // dropped here because start tests only care about the launch arguments.
            let plan = build_redis_server_start_plan(paths, bind, port, password)?;
            self.redis_server_starts.push(CommandPlan {
                program: plan.program,
                args: plan.args,
            });
            self.redis_server_result.take().unwrap_or_else(Self::ok)
        })
    }

    fn ping<'a>(&'a mut self, url: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            self.pings.push(url.to_string());
            self.ping_result.take().unwrap_or_else(Self::ok)
        })
    }

    fn run_command<'a>(
        &'a mut self,
        plan: &'a CommandPlan,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
            self.commands.push(plan.clone());
            // Consume results in FIFO order; default to Ok(()) once the
            // pre-loaded queue is exhausted.
            if self.command_results.is_empty() {
                Ok(())
            } else {
                self.command_results.remove(0)
            }
        })
    }

    fn container_exists<'a>(
        &'a mut self,
        name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + 'a>> {
        Box::pin(async move {
            self.container_exists_calls.push(name.to_string());
            // Default: container is present (triggers the stop path).
            self.container_exists_result.take().unwrap_or(Ok(true))
        })
    }

    fn process_executable<'a>(
        &'a mut self,
        pid: u32,
    ) -> Pin<Box<dyn Future<Output = Result<Option<String>>> + 'a>> {
        Box::pin(async move {
            self.process_executable_calls.push(pid);
            // Default: the process at `pid` is `redis-server` (safe to kill).
            self.process_executable_result
                .take()
                .unwrap_or_else(|| Ok(Some("redis-server".to_string())))
        })
    }
}

/// Creates a temporary directory and derives a [`ConfigPaths`] rooted inside
/// it, simulating `~/.magi` state without touching the real home directory.
///
/// The caller must keep the returned [`tempfile::TempDir`] alive for the
/// duration of the test; dropping it removes the directory.
fn temp_paths() -> (tempfile::TempDir, ConfigPaths) {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = ConfigPaths::from_home(temp.path());
    (temp, paths)
}

/// Verifies that the Docker start plan assembles the correct `docker run`
/// arguments: detached mode, container name, port binding, data-volume mount,
/// config-file mount, and the `redis:7-alpine` image — and that the plaintext
/// password is never embedded directly in the argument list.
#[test]
fn docker_start_plan_includes_container_network_storage_image_and_auth() {
    let (_temp, paths) = temp_paths();

    let plan = build_docker_start_plan(&paths, "127.0.0.1", 6379, "secret").expect("docker plan");

    assert_eq!(plan.program, "docker");
    assert!(plan.args.windows(2).any(|window| window == ["run", "-d"]));
    assert!(plan
        .args
        .windows(2)
        .any(|window| window == ["--name", "magi-redis"]));
    assert!(plan
        .args
        .windows(2)
        .any(|window| window == ["-p", "127.0.0.1:6379:6379"]));
    assert!(plan.args.windows(2).any(|window| {
        window[0] == "-v" && window[1] == format!("{}:/data", paths.redis_data_dir.display())
    }));
    assert!(plan.args.iter().any(|arg| arg == "redis:7-alpine"));
    assert!(plan
        .args
        .windows(2)
        .any(|window| window == ["redis-server", "/usr/local/etc/redis/redis.conf"]));
    // Password must only appear inside the Redis config file mounted into the
    // container, never as a plain CLI argument visible in `ps` output.
    assert!(!plan.args.iter().any(|arg| arg.contains("secret")));
    assert!(plan.args.windows(2).any(|window| window[0] == "-v"
        && window[1].contains("redis.conf:/usr/local/etc/redis/redis.conf:ro")));
}

/// Verifies that the `redis-server` start plan writes a config file containing
/// the bind address, port, AOF persistence, password (`requirepass`), data
/// directory, and PID file path — and that `redis-server` is invoked with
/// that config file as its sole argument.
#[test]
fn redis_server_start_plan_includes_config_file_with_bind_port_aof_and_auth() {
    let (_temp, paths) = temp_paths();

    let plan =
        build_redis_server_start_plan(&paths, "127.0.0.1", 6380, "secret").expect("server plan");

    assert_eq!(plan.program, "redis-server");
    assert_eq!(plan.args, vec![plan.config_file.display().to_string()]);
    assert!(plan.config_file.starts_with(&paths.redis_dir));
    assert!(plan.config_contents.contains("bind 127.0.0.1\n"));
    assert!(plan.config_contents.contains("port 6380\n"));
    assert!(plan.config_contents.contains("appendonly yes\n"));
    assert!(plan.config_contents.contains("requirepass secret\n"));
    assert!(plan
        .config_contents
        .contains(&format!("dir {}\n", paths.redis_data_dir.display())));
    assert!(plan.config_contents.contains(&format!(
        "pidfile {}\n",
        redis_server_pid_file(&paths).display()
    )));
}

/// A LAN-bind address (`0.0.0.0`) without a password must be rejected at plan
/// construction time, before any process is spawned.
#[test]
fn redis_server_start_plan_rejects_lan_bind_without_password() {
    let (_temp, paths) = temp_paths();

    let error = build_redis_server_start_plan(&paths, "0.0.0.0", 6379, "")
        .expect_err("LAN bind without password should fail");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("password")));
}

/// Same safety check as the `redis-server` variant — Docker mode must also
/// refuse to expose Redis on a LAN interface without a password.
#[test]
fn docker_start_plan_rejects_lan_bind_without_password() {
    let (_temp, paths) = temp_paths();

    let error = build_docker_start_plan(&paths, "0.0.0.0", 6379, "")
        .expect_err("LAN bind without password should fail");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("password")));
}

/// A localhost bind with an auto-generated password must be accepted, and the
/// generated password must never appear verbatim in the `docker run` arguments.
#[test]
fn localhost_bind_with_generated_password_is_accepted() {
    let (_temp, paths) = temp_paths();
    // `password_for_start(None)` generates a fresh random password.
    let password = password_for_start(None);

    let plan = build_docker_start_plan(&paths, "127.0.0.1", 6379, &password)
        .expect("localhost with generated password");

    assert!(!password.is_empty());
    assert!(!plan.args.iter().any(|arg| arg.contains(&password)));
}

/// When an existing Redis URL is provided, `password_for_start` must extract
/// and reuse its password instead of generating a new one, preserving access
/// for clients that already hold the original URL.
#[test]
fn password_extraction_reuses_existing_redis_url_password() {
    let url = "redis://:existing-password@127.0.0.1:6379";

    assert_eq!(
        extract_password_from_redis_url(url).as_deref(),
        Some("existing-password")
    );
    assert_eq!(password_for_start(Some(url)), "existing-password");
}

/// `build_redis_url` must produce a well-formed `redis://:password@host:port`
/// URL that clients can pass directly to the Redis client library.
#[test]
fn redis_url_includes_password_and_localhost_port() {
    let url = build_redis_url("127.0.0.1", 6379, "secret").expect("url");

    assert_eq!(url, "redis://:secret@127.0.0.1:6379");
}

/// The `redis-server` stop plan must use `kill` with the PID file path so
/// the caller can read the PID and signal the correct process.
#[test]
fn redis_server_stop_plan_uses_pid_file() {
    let (_temp, paths) = temp_paths();

    let plan = redis_server_stop_plan(&paths);

    assert_eq!(plan.program, "kill");
    assert_eq!(plan.pid_file, redis_server_pid_file(&paths));
}

/// `redact_command_for_diagnostics` must replace the value that follows a
/// sensitive flag (e.g. `--requirepass`) with `<redacted>` so that log output
/// never leaks credentials, while still showing the flag name itself.
#[test]
fn command_diagnostics_redact_sensitive_argument_values() {
    let plan = CommandPlan {
        program: "redis-server".to_string(),
        args: vec![
            "--appendonly".to_string(),
            "yes".to_string(),
            "--requirepass".to_string(),
            "secret".to_string(),
        ],
    };

    let diagnostic = redact_command_for_diagnostics(&plan);

    assert!(diagnostic.contains("--requirepass <redacted>"));
    assert!(!diagnostic.contains("secret"));
}

/// On Unix, `write_private_redis_config_file` must atomically replace any
/// existing symlink at the target path with a regular file owned by the
/// current process (mode `0o600`).  This prevents a malicious symlink from
/// redirecting the password-containing config to an attacker-controlled path.
#[cfg(unix)]
#[test]
fn private_redis_config_write_replaces_symlink_with_private_file() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let (_temp, paths) = temp_paths();
    std::fs::create_dir_all(&paths.redis_dir).expect("redis dir");

    // Pre-condition: an adversarial symlink points the config path at a file
    // outside the magi state directory.
    let config_file = paths.redis_dir.join("redis.conf");
    let outside_file = paths.root.join("outside.conf");
    std::fs::create_dir_all(&paths.root).expect("root dir");
    std::fs::write(&outside_file, "outside\n").expect("outside file");
    symlink(&outside_file, &config_file).expect("config symlink");

    write_private_redis_config_file(&config_file, b"requirepass secret\n").expect("write config");

    let outside_contents = std::fs::read_to_string(&outside_file).expect("outside contents");
    // Use symlink_metadata (not metadata) so we see the symlink itself, not its target.
    let config_type = std::fs::symlink_metadata(&config_file)
        .expect("config metadata")
        .file_type();
    // Mask to permission bits only; ignore setuid/setgid/sticky bits.
    let config_mode = std::fs::metadata(&config_file)
        .expect("config mode")
        .permissions()
        .mode()
        & 0o777;

    // The file pointed to by the original symlink must be untouched.
    assert_eq!(outside_contents, "outside\n");
    // The config path must now be a regular file, not a symlink.
    assert!(!config_type.is_symlink());
    assert_eq!(
        std::fs::read_to_string(&config_file).expect("config contents"),
        "requirepass secret\n"
    );
    // 0o600: owner read+write only — no group or world access to the password file.
    assert_eq!(config_mode, 0o600);
}

/// When Docker succeeds, `start_with_runtime` must record `Docker` as the
/// runtime mode in both the returned config and the persisted on-disk config,
/// reuse the existing Redis URL (rather than generating a new password), and
/// must NOT attempt to launch `redis-server`.
#[tokio::test]
async fn start_uses_docker_when_docker_start_succeeds_and_saves_runtime_config() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
    // Pre-supply a URL so the test can assert it is preserved verbatim.
    config.redis.url = Some("redis://:reused@127.0.0.1:6379".to_string());
    let mut runtime = FakeRuntime::default();

    let saved = start_with_runtime(&paths, config, false, None, &mut runtime)
        .await
        .expect("start");

    assert_eq!(saved.redis.mode, RedisMode::Docker);
    assert_eq!(saved.redis.bind, "127.0.0.1");
    assert_eq!(
        saved.redis.url.as_deref(),
        Some("redis://:reused@127.0.0.1:6379")
    );
    // Reload from disk to confirm the config was actually persisted, not just
    // returned in memory.
    let loaded = AppConfig::load_from_paths(&paths).expect("load saved config");
    assert_eq!(loaded.redis.mode, RedisMode::Docker);
    assert_eq!(loaded.redis.bind, "127.0.0.1");
    assert_eq!(
        loaded.redis.url.as_deref(),
        Some("redis://:reused@127.0.0.1:6379")
    );
    assert_eq!(runtime.docker_starts.len(), 1);
    assert!(runtime.redis_server_starts.is_empty());
}

/// When Docker fails, `start_with_runtime` must retry with `redis-server`,
/// record `RedisServer` as the mode in both the in-memory and persisted
/// config, and generate a fresh password (URL starts with `redis://:`).
#[tokio::test]
async fn start_falls_back_to_redis_server_when_docker_fails_and_saves_runtime_config() {
    let (_temp, paths) = temp_paths();
    let config = AppConfig::default();
    // Docker is pre-configured to fail so the fallback path is exercised.
    let mut runtime = FakeRuntime {
        docker_result: Some(FakeRuntime::fail("docker failed")),
        ..FakeRuntime::default()
    };

    let saved = start_with_runtime(
        &paths,
        config,
        false,
        Some("127.0.0.1".to_string()),
        &mut runtime,
    )
    .await
    .expect("fallback start");

    assert_eq!(saved.redis.mode, RedisMode::RedisServer);
    assert_eq!(saved.redis.bind, "127.0.0.1");
    assert!(saved
        .redis
        .url
        .as_deref()
        .expect("redis url")
        .starts_with("redis://:"));
    let loaded = AppConfig::load_from_paths(&paths).expect("load saved config");
    assert_eq!(loaded.redis.mode, RedisMode::RedisServer);
    assert_eq!(loaded.redis.bind, "127.0.0.1");
    assert_eq!(loaded.redis.url, saved.redis.url);
    assert_eq!(runtime.docker_starts.len(), 1);
    assert_eq!(runtime.redis_server_starts.len(), 1);
}

/// When both Docker and `redis-server` fail, `start_with_runtime` must
/// propagate the `redis-server` error (the last attempted backend) and must
/// have recorded one attempt for each backend.
#[tokio::test]
async fn start_returns_error_when_docker_and_redis_server_fail() {
    let (_temp, paths) = temp_paths();
    let config = AppConfig::default();
    let mut runtime = FakeRuntime {
        docker_result: Some(FakeRuntime::fail("docker failed")),
        redis_server_result: Some(FakeRuntime::fail("redis-server failed")),
        ..FakeRuntime::default()
    };

    let error = start_with_runtime(&paths, config, false, None, &mut runtime)
        .await
        .expect_err("both starts fail");

    assert!(
        matches!(error, MagiError::CommandFailed(message) if message.contains("redis-server failed"))
    );
    assert_eq!(runtime.docker_starts.len(), 1);
    assert_eq!(runtime.redis_server_starts.len(), 1);
}

#[tokio::test]
async fn status_pings_configured_redis_url() {
    let mut config = AppConfig::default();
    config.redis.url = Some("redis://:secret@127.0.0.1:6379".to_string());
    let mut runtime = FakeRuntime::default();

    status_with_runtime(&config, &mut runtime)
        .await
        .expect("status");

    assert_eq!(runtime.pings, vec!["redis://:secret@127.0.0.1:6379"]);
}

#[tokio::test]
async fn status_without_redis_url_fails_clearly() {
    let config = AppConfig::default();
    let mut runtime = FakeRuntime::default();

    let error = status_with_runtime(&config, &mut runtime)
        .await
        .expect_err("missing url");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("redis.url")));
    assert!(runtime.pings.is_empty());
}

#[tokio::test]
async fn stop_docker_mode_runs_docker_remove_force_plan() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::Docker;
    let mut runtime = FakeRuntime::default();

    stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect("stop docker");

    assert_eq!(runtime.commands, vec![docker_stop_plan()]);
    assert_eq!(
        runtime.container_exists_calls,
        vec!["magi-redis".to_string()]
    );
}

#[tokio::test]
async fn stop_docker_mode_missing_container_is_noop() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::Docker;
    let mut runtime = FakeRuntime {
        container_exists_result: Some(Ok(false)),
        ..FakeRuntime::default()
    };

    stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect("stop docker without container");

    assert!(runtime.commands.is_empty());
    assert_eq!(
        runtime.container_exists_calls,
        vec!["magi-redis".to_string()]
    );
}

/// Happy path: a valid PID file containing a process whose executable name
/// matches `redis-server` must result in a `kill` command for that PID, and
/// the PID file must be removed afterward to avoid stale state on the next start.
#[tokio::test]
async fn stop_redis_server_mode_reads_pid_file_and_kills_pid() {
    let (_temp, paths) = temp_paths();
    // Create the run directory and write a well-formed PID file.
    std::fs::create_dir_all(&paths.run_dir).expect("run dir");
    std::fs::write(redis_server_pid_file(&paths), "12345\n").expect("pid file");
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::RedisServer;
    let mut runtime = FakeRuntime::default();

    stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect("stop redis-server");

    assert_eq!(
        runtime.commands,
        vec![CommandPlan {
            program: "kill".to_string(),
            args: vec!["12345".to_string()],
        }]
    );
    assert_eq!(runtime.process_executable_calls, vec![12345]);
    assert!(
        !redis_server_pid_file(&paths).exists(),
        "pid file should be removed after a successful stop"
    );
}

/// When the PID in the file refers to a process that no longer exists
/// (`process_executable` returns `None`), `stop_with_runtime` must silently
/// remove the stale PID file without attempting to send a signal.
#[tokio::test]
async fn stop_redis_server_mode_removes_stale_pid_file_when_process_is_gone() {
    let (_temp, paths) = temp_paths();
    std::fs::create_dir_all(&paths.run_dir).expect("run dir");
    std::fs::write(redis_server_pid_file(&paths), "12345\n").expect("pid file");
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::RedisServer;
    // `Ok(None)` means no process exists for this PID (already exited).
    let mut runtime = FakeRuntime {
        process_executable_result: Some(Ok(None)),
        ..FakeRuntime::default()
    };

    stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect("stop redis-server with stale pid");

    assert!(runtime.commands.is_empty());
    assert!(
        !redis_server_pid_file(&paths).exists(),
        "stale pid file should be removed"
    );
}

/// If the process at the recorded PID is NOT `redis-server` (e.g. the OS
/// recycled the PID and assigned it to an unrelated process), `stop_with_runtime`
/// must refuse to send a signal and leave the PID file intact so the operator
/// can investigate.
#[tokio::test]
async fn stop_redis_server_mode_refuses_to_kill_foreign_process() {
    let (_temp, paths) = temp_paths();
    std::fs::create_dir_all(&paths.run_dir).expect("run dir");
    std::fs::write(redis_server_pid_file(&paths), "12345\n").expect("pid file");
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::RedisServer;
    // The executable at PID 12345 is `bash`, not `redis-server` — PID was recycled.
    let mut runtime = FakeRuntime {
        process_executable_result: Some(Ok(Some("bash".to_string()))),
        ..FakeRuntime::default()
    };

    let error = stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect_err("foreign pid must not be killed");

    assert!(
        matches!(error, MagiError::CommandFailed(message) if message.contains("refusing to kill"))
    );
    assert!(runtime.commands.is_empty());
    assert!(
        redis_server_pid_file(&paths).exists(),
        "pid file must be preserved when the process is not ours"
    );
}

#[tokio::test]
async fn stop_redis_server_mode_missing_pid_file_is_noop() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::RedisServer;
    let mut runtime = FakeRuntime::default();

    stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect("stop redis-server without pid file");

    assert!(runtime.commands.is_empty());
}

#[tokio::test]
async fn stop_redis_server_mode_empty_pid_file_is_noop() {
    let (_temp, paths) = temp_paths();
    std::fs::create_dir_all(&paths.run_dir).expect("run dir");
    std::fs::write(redis_server_pid_file(&paths), "\n").expect("pid file");
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::RedisServer;
    let mut runtime = FakeRuntime::default();

    stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect("stop redis-server with empty pid file");

    assert!(runtime.commands.is_empty());
}

#[tokio::test]
async fn stop_redis_server_mode_whitespace_only_pid_file_is_noop() {
    let (_temp, paths) = temp_paths();
    std::fs::create_dir_all(&paths.run_dir).expect("run dir");
    std::fs::write(redis_server_pid_file(&paths), "  \n\t").expect("pid file");
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::RedisServer;
    let mut runtime = FakeRuntime::default();

    stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect("stop redis-server with whitespace pid file");

    assert!(runtime.commands.is_empty());
}

#[tokio::test]
async fn stop_redis_server_mode_rejects_negative_pid_file() {
    let (_temp, paths) = temp_paths();
    std::fs::create_dir_all(&paths.run_dir).expect("run dir");
    std::fs::write(redis_server_pid_file(&paths), "-1\n").expect("pid file");
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::RedisServer;
    let mut runtime = FakeRuntime::default();

    let error = stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect_err("negative pid should fail");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("pid")));
    assert!(runtime.commands.is_empty());
}

#[tokio::test]
async fn stop_redis_server_mode_rejects_non_numeric_pid_file() {
    let (_temp, paths) = temp_paths();
    std::fs::create_dir_all(&paths.run_dir).expect("run dir");
    std::fs::write(redis_server_pid_file(&paths), "abc\n").expect("pid file");
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::RedisServer;
    let mut runtime = FakeRuntime::default();

    let error = stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect_err("non-numeric pid should fail");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("pid")));
    assert!(runtime.commands.is_empty());
}

#[tokio::test]
async fn stop_external_mode_is_noop() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
    config.redis.mode = RedisMode::External;
    let mut runtime = FakeRuntime::default();

    stop_with_runtime(&paths, &config, &mut runtime)
        .await
        .expect("stop external");

    assert!(runtime.commands.is_empty());
}
