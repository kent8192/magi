use std::time::Duration;

use magi::error::MagiError;
use magi::invite::{
    create_invite_with_url, join_with_url, list_invites_with_url, parse_ttl,
    revoke_invite_with_url, token_hash,
};
use magi::model::RedisKeys;
use magi::team::{create_team_with_url, list_members_with_url, register_agent_with_url};
use redis::AsyncCommands;

fn redis_url_from_values(url: Option<String>, require_redis_tests: bool) -> Option<String> {
    let url = url.filter(|url| !url.trim().is_empty());

    if url.is_none() && require_redis_tests {
        panic!("MAGI_REQUIRE_REDIS_TESTS=1 requires MAGI_TEST_REDIS_URL to be set");
    }

    url
}

fn redis_url_from_env() -> Option<String> {
    redis_url_from_values(
        std::env::var("MAGI_TEST_REDIS_URL").ok(),
        std::env::var("MAGI_REQUIRE_REDIS_TESTS").as_deref() == Ok("1"),
    )
}

fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuidish())
}

fn uuidish() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    )
}

async fn redis_connection(url: &str) -> redis::aio::MultiplexedConnection {
    redis::Client::open(url)
        .expect("redis client")
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection")
}

fn is_clear_revoked_rejection(error: MagiError) -> bool {
    match error {
        MagiError::NotFound(message) => message.contains("invite token"),
        MagiError::InvalidConfig(message) => {
            message.contains("revoked") || message.contains("invalid")
        }
        _ => false,
    }
}

#[test]
fn redis_url_helper_skips_when_url_is_missing_and_not_required() {
    assert_eq!(redis_url_from_values(None, false), None);
    assert_eq!(redis_url_from_values(Some("   ".to_string()), false), None);
}

#[test]
#[should_panic(expected = "MAGI_REQUIRE_REDIS_TESTS=1 requires MAGI_TEST_REDIS_URL to be set")]
fn redis_url_helper_panics_when_redis_tests_are_required_without_url() {
    let _ = redis_url_from_values(None, true);
}

#[test]
fn ttl_parser_accepts_h_m_s_suffixes() {
    assert_eq!(parse_ttl("2h").expect("hours"), Duration::from_secs(7200));
    assert_eq!(parse_ttl("15m").expect("minutes"), Duration::from_secs(900));
    assert_eq!(parse_ttl("45s").expect("seconds"), Duration::from_secs(45));
}

#[test]
fn ttl_parser_rejects_invalid_values() {
    assert!(parse_ttl("").is_err());
    assert!(parse_ttl("0s").is_err());
    assert!(parse_ttl("10d").is_err());
    assert!(parse_ttl("abc").is_err());
}

#[tokio::test]
async fn create_invite_join_list_members_and_revoke_flow() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-flow");
    let owner = unique_name("owner");
    let agent = unique_name("agent");

    create_team_with_url(&url, &team, &owner).await.unwrap();
    let invite = create_invite_with_url(&url, &team, &owner, Duration::from_secs(60))
        .await
        .unwrap();

    join_with_url(&url, &invite.token, &agent, "codex", "/tmp/project-a")
        .await
        .unwrap();

    let members = list_members_with_url(&url, &team).await.unwrap();
    assert!(members.iter().any(|member| member.name == owner));
    assert!(members.iter().any(|member| member.name == agent));

    revoke_invite_with_url(&url, &team, &invite.invite_id)
        .await
        .unwrap();
    let error = join_with_url(
        &url,
        &invite.token,
        &unique_name("late"),
        "codex",
        "/tmp/project-b",
    )
    .await
    .expect_err("revoked invite should reject later joins");
    assert!(is_clear_revoked_rejection(error));
}

#[tokio::test]
async fn re_registering_same_agent_project_does_not_duplicate_members() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-idempotent");
    let agent = unique_name("agent");

    create_team_with_url(&url, &team, &agent).await.unwrap();
    register_agent_with_url(&url, &team, &agent, "codex", "/tmp/project-a")
        .await
        .unwrap();
    register_agent_with_url(&url, &team, &agent, "codex", "/tmp/project-a")
        .await
        .unwrap();

    let members = list_members_with_url(&url, &team).await.unwrap();
    assert_eq!(
        members.iter().filter(|member| member.name == agent).count(),
        1
    );

    let keys = RedisKeys::new(&team);
    let mut connection = redis_connection(&url).await;
    let registrations: Vec<String> = connection
        .smembers(keys.registrations(&agent))
        .await
        .expect("registrations");
    assert_eq!(registrations, vec!["codex:/tmp/project-a".to_string()]);
}

#[tokio::test]
async fn invalid_invite_token_is_rejected() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };

    let error = join_with_url(&url, "not-a-real-token", "agent", "codex", "/tmp/project")
        .await
        .expect_err("invalid invite token should fail");

    assert!(matches!(error, MagiError::NotFound(message) if message.contains("invite token")));
}

#[tokio::test]
async fn revoked_invite_is_rejected() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-revoked");
    let owner = unique_name("owner");

    create_team_with_url(&url, &team, &owner).await.unwrap();
    let invite = create_invite_with_url(&url, &team, &owner, Duration::from_secs(60))
        .await
        .unwrap();
    revoke_invite_with_url(&url, &team, &invite.invite_id)
        .await
        .unwrap();

    let error = join_with_url(&url, &invite.token, "agent", "codex", "/tmp/project")
        .await
        .expect_err("revoked invite should fail");

    assert!(is_clear_revoked_rejection(error));
}

#[tokio::test]
async fn revoked_marker_is_rejected_when_lookup_still_exists() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-revoked-marker");
    let owner = unique_name("owner");

    create_team_with_url(&url, &team, &owner).await.unwrap();
    let invite = create_invite_with_url(&url, &team, &owner, Duration::from_secs(60))
        .await
        .unwrap();

    let keys = RedisKeys::new(&team);
    let mut connection = redis_connection(&url).await;
    let _: () = connection
        .hset(keys.invite(&invite.invite_id), "revoked_at", "1")
        .await
        .expect("set revoked marker");

    let error = join_with_url(&url, &invite.token, "agent", "codex", "/tmp/project")
        .await
        .expect_err("revoked marker should fail");

    assert!(
        matches!(&error, MagiError::InvalidConfig(message) if message.contains("revoked")),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn wrong_team_revoke_fails_and_keeps_lookup() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team_a = unique_name("team-boundary-a");
    let team_b = unique_name("team-boundary-b");
    let owner = unique_name("owner");

    create_team_with_url(&url, &team_a, &owner).await.unwrap();
    create_team_with_url(&url, &team_b, &owner).await.unwrap();
    let invite = create_invite_with_url(&url, &team_a, &owner, Duration::from_secs(60))
        .await
        .unwrap();

    let error = revoke_invite_with_url(&url, &team_b, &invite.invite_id)
        .await
        .expect_err("wrong team must not revoke invite");
    assert!(
        matches!(&error, MagiError::NotFound(message) if message.contains("invite")),
        "unexpected error: {error}"
    );

    let mut connection = redis_connection(&url).await;
    let lookup_key = RedisKeys::invite_token(&token_hash(&invite.token));
    let lookup_exists: bool = connection.exists(lookup_key).await.expect("lookup exists");
    let revoked_at: Option<String> = connection
        .hget(
            RedisKeys::new(&team_a).invite(&invite.invite_id),
            "revoked_at",
        )
        .await
        .expect("revoked marker");
    assert!(lookup_exists);
    assert_eq!(revoked_at, None);
}

#[tokio::test]
async fn list_invites_filters_out_invites_from_other_teams() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team_a = unique_name("team-list-a");
    let team_b = unique_name("team-list-b");
    let owner = unique_name("owner");

    create_team_with_url(&url, &team_a, &owner).await.unwrap();
    create_team_with_url(&url, &team_b, &owner).await.unwrap();
    let invite = create_invite_with_url(&url, &team_a, &owner, Duration::from_secs(60))
        .await
        .unwrap();

    let keys_b = RedisKeys::new(&team_b);
    let mut connection = redis_connection(&url).await;
    let _: () = connection
        .sadd(format!("{}:invites", keys_b.team()), &invite.invite_id)
        .await
        .expect("pollute team B invite set");

    let invites = list_invites_with_url(&url, &team_b).await.unwrap();
    assert!(
        invites
            .iter()
            .all(|listed| listed.invite_id != invite.invite_id),
        "team B must not list team A invite"
    );
}

#[tokio::test]
async fn expired_invite_is_rejected() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-expired");
    let owner = unique_name("owner");

    create_team_with_url(&url, &team, &owner).await.unwrap();
    let invite = create_invite_with_url(&url, &team, &owner, Duration::from_secs(1))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let error = join_with_url(&url, &invite.token, "agent", "codex", "/tmp/project")
        .await
        .expect_err("expired invite should fail");

    assert!(matches!(error, MagiError::NotFound(message) if message.contains("invite token")));
}

#[tokio::test]
async fn max_uses_one_prevents_second_join() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-max-uses");
    let owner = unique_name("owner");

    create_team_with_url(&url, &team, &owner).await.unwrap();
    let invite = create_invite_with_url(&url, &team, &owner, Duration::from_secs(60))
        .await
        .unwrap();

    let keys = RedisKeys::new(&team);
    let mut connection = redis_connection(&url).await;
    let _: () = connection
        .hset(keys.invite(&invite.invite_id), "max_uses", 1)
        .await
        .expect("set max uses");

    join_with_url(&url, &invite.token, "agent-one", "codex", "/tmp/project-a")
        .await
        .unwrap();
    let error = join_with_url(&url, &invite.token, "agent-two", "codex", "/tmp/project-b")
        .await
        .expect_err("max uses should reject second join");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("maximum uses")));
}

#[tokio::test]
async fn max_uses_one_allows_exactly_one_concurrent_join() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-max-uses-race");
    let owner = unique_name("owner");

    create_team_with_url(&url, &team, &owner).await.unwrap();
    let invite = create_invite_with_url(&url, &team, &owner, Duration::from_secs(60))
        .await
        .unwrap();

    let keys = RedisKeys::new(&team);
    let mut connection = redis_connection(&url).await;
    let _: () = connection
        .hset(keys.invite(&invite.invite_id), "max_uses", 1)
        .await
        .expect("set max uses");

    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..24 {
        let url = url.clone();
        let token = invite.token.clone();
        tasks.spawn(async move {
            join_with_url(
                &url,
                &token,
                &format!("racer-{index}"),
                "codex",
                &format!("/tmp/project-{index}"),
            )
            .await
        });
    }

    let mut successes = 0;
    let mut failures = 0;
    while let Some(result) = tasks.join_next().await {
        match result.expect("join task") {
            Ok(_) => successes += 1,
            Err(MagiError::InvalidConfig(message)) if message.contains("maximum uses") => {
                failures += 1
            }
            Err(error) => panic!("unexpected join error: {error}"),
        }
    }

    let used_count: u64 = connection
        .hget(keys.invite(&invite.invite_id), "used_count")
        .await
        .expect("used count");
    assert_eq!(successes, 1);
    assert_eq!(failures, 23);
    assert_eq!(used_count, 1);
}

#[tokio::test]
async fn invite_stores_only_token_hash_and_revoke_deletes_lookup() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-security");
    let owner = unique_name("owner");

    create_team_with_url(&url, &team, &owner).await.unwrap();
    let invite = create_invite_with_url(&url, &team, &owner, Duration::from_secs(60))
        .await
        .unwrap();

    let keys = RedisKeys::new(&team);
    let hash = token_hash(&invite.token);
    let lookup_key = RedisKeys::invite_token(&hash);
    let mut connection = redis_connection(&url).await;

    let stored_token: Option<String> = connection
        .hget(keys.invite(&invite.invite_id), "token")
        .await
        .expect("token field");
    let stored_hash: String = connection
        .hget(keys.invite(&invite.invite_id), "token_hash")
        .await
        .expect("token hash");
    let lookup_team_and_id: Option<String> = connection.get(&lookup_key).await.expect("lookup");

    assert_eq!(stored_token, None);
    assert_eq!(stored_hash, hash);
    assert_eq!(lookup_team_and_id, Some(keys.invite(&invite.invite_id)));

    revoke_invite_with_url(&url, &team, &invite.invite_id)
        .await
        .unwrap();
    let exists_after_revoke: bool = connection.exists(&lookup_key).await.expect("exists");
    assert!(!exists_after_revoke);
}
