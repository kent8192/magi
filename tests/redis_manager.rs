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

#[derive(Debug, Default)]
struct FakeRuntime {
    docker_result: Option<Result<()>>,
    redis_server_result: Option<Result<()>>,
    ping_result: Option<Result<()>>,
    command_results: Vec<Result<()>>,
    docker_starts: Vec<CommandPlan>,
    redis_server_starts: Vec<CommandPlan>,
    pings: Vec<String>,
    commands: Vec<CommandPlan>,
}

impl FakeRuntime {
    fn ok() -> Result<()> {
        Ok(())
    }

    fn fail(message: &str) -> Result<()> {
        Err(MagiError::CommandFailed(message.to_string()))
    }
}

impl RedisRuntime for FakeRuntime {
    fn start_docker<'a>(
        &'a mut self,
        paths: &'a ConfigPaths,
        bind: &'a str,
        port: u16,
        password: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + 'a>> {
        Box::pin(async move {
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
            if self.command_results.is_empty() {
                Ok(())
            } else {
                self.command_results.remove(0)
            }
        })
    }
}

fn temp_paths() -> (tempfile::TempDir, ConfigPaths) {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = ConfigPaths::from_home(temp.path());
    (temp, paths)
}

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
    assert!(!plan.args.iter().any(|arg| arg.contains("secret")));
    assert!(plan.args.windows(2).any(|window| window[0] == "-v"
        && window[1].contains("redis.conf:/usr/local/etc/redis/redis.conf:ro")));
}

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

#[test]
fn redis_server_start_plan_rejects_lan_bind_without_password() {
    let (_temp, paths) = temp_paths();

    let error = build_redis_server_start_plan(&paths, "0.0.0.0", 6379, "")
        .expect_err("LAN bind without password should fail");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("password")));
}

#[test]
fn docker_start_plan_rejects_lan_bind_without_password() {
    let (_temp, paths) = temp_paths();

    let error = build_docker_start_plan(&paths, "0.0.0.0", 6379, "")
        .expect_err("LAN bind without password should fail");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("password")));
}

#[test]
fn localhost_bind_with_generated_password_is_accepted() {
    let (_temp, paths) = temp_paths();
    let password = password_for_start(None);

    let plan = build_docker_start_plan(&paths, "127.0.0.1", 6379, &password)
        .expect("localhost with generated password");

    assert!(!password.is_empty());
    assert!(!plan.args.iter().any(|arg| arg.contains(&password)));
}

#[test]
fn password_extraction_reuses_existing_redis_url_password() {
    let url = "redis://:existing-password@127.0.0.1:6379";

    assert_eq!(
        extract_password_from_redis_url(url).as_deref(),
        Some("existing-password")
    );
    assert_eq!(password_for_start(Some(url)), "existing-password");
}

#[test]
fn redis_url_includes_password_and_localhost_port() {
    let url = build_redis_url("127.0.0.1", 6379, "secret").expect("url");

    assert_eq!(url, "redis://:secret@127.0.0.1:6379");
}

#[test]
fn redis_server_stop_plan_uses_pid_file() {
    let (_temp, paths) = temp_paths();

    let plan = redis_server_stop_plan(&paths);

    assert_eq!(plan.program, "kill");
    assert_eq!(plan.pid_file, redis_server_pid_file(&paths));
}

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

#[cfg(unix)]
#[test]
fn private_redis_config_write_replaces_symlink_with_private_file() {
    use std::os::unix::fs::{symlink, PermissionsExt};

    let (_temp, paths) = temp_paths();
    std::fs::create_dir_all(&paths.redis_dir).expect("redis dir");

    let config_file = paths.redis_dir.join("redis.conf");
    let outside_file = paths.root.join("outside.conf");
    std::fs::create_dir_all(&paths.root).expect("root dir");
    std::fs::write(&outside_file, "outside\n").expect("outside file");
    symlink(&outside_file, &config_file).expect("config symlink");

    write_private_redis_config_file(&config_file, b"requirepass secret\n").expect("write config");

    let outside_contents = std::fs::read_to_string(&outside_file).expect("outside contents");
    let config_type = std::fs::symlink_metadata(&config_file)
        .expect("config metadata")
        .file_type();
    let config_mode = std::fs::metadata(&config_file)
        .expect("config mode")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(outside_contents, "outside\n");
    assert!(!config_type.is_symlink());
    assert_eq!(
        std::fs::read_to_string(&config_file).expect("config contents"),
        "requirepass secret\n"
    );
    assert_eq!(config_mode, 0o600);
}

#[tokio::test]
async fn start_uses_docker_when_docker_start_succeeds_and_saves_runtime_config() {
    let (_temp, paths) = temp_paths();
    let mut config = AppConfig::default();
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

#[tokio::test]
async fn start_falls_back_to_redis_server_when_docker_fails_and_saves_runtime_config() {
    let (_temp, paths) = temp_paths();
    let config = AppConfig::default();
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
}

#[tokio::test]
async fn stop_redis_server_mode_reads_pid_file_and_kills_pid() {
    let (_temp, paths) = temp_paths();
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
