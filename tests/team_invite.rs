//! Integration tests for the team-creation and invite-based onboarding flows.
//!
//! These tests exercise the full Redis-backed lifecycle of a magi team invite:
//! creation, joining, listing members, revocation, expiry, max-use enforcement,
//! and security properties (token hash storage, lookup-key cleanup).
//!
//! # Gating
//!
//! All Redis-backed tests are skipped when `MAGI_TEST_REDIS_URL` is unset.
//! Set `MAGI_REQUIRE_REDIS_TESTS=1` to turn the skip into a hard failure, which
//! is useful in CI environments where Redis is expected to be available.
//!
//! ```text
//! MAGI_TEST_REDIS_URL=redis://127.0.0.1:6379 cargo test -p magi --test team_invite
//! ```

use std::time::Duration;

use magi::error::MagiError;
use magi::invite::{
    create_invite_with_url, join_with_url, list_invites_with_url, parse_ttl,
    revoke_invite_with_url, token_hash,
};
use magi::model::RedisKeys;
use magi::team::{create_team_with_url, list_members_with_url, register_agent_with_url};
use redis::AsyncCommands;

/// Resolves the Redis URL from the supplied `url` value and the `require_redis_tests` flag.
///
/// Returns `None` when `url` is absent or blank and `require_redis_tests` is `false`,
/// allowing callers to skip the test gracefully.
///
/// # Panics
///
/// Panics when `url` is absent or blank and `require_redis_tests` is `true`, so that
/// CI environments which set `MAGI_REQUIRE_REDIS_TESTS=1` surface a clear failure instead
/// of silently skipping all Redis-backed tests.
fn redis_url_from_values(url: Option<String>, require_redis_tests: bool) -> Option<String> {
    let url = url.filter(|url| !url.trim().is_empty());

    if url.is_none() && require_redis_tests {
        panic!("MAGI_REQUIRE_REDIS_TESTS=1 requires MAGI_TEST_REDIS_URL to be set");
    }

    url
}

/// Reads `MAGI_TEST_REDIS_URL` and `MAGI_REQUIRE_REDIS_TESTS` from the process environment
/// and delegates to [`redis_url_from_values`].
fn redis_url_from_env() -> Option<String> {
    redis_url_from_values(
        std::env::var("MAGI_TEST_REDIS_URL").ok(),
        std::env::var("MAGI_REQUIRE_REDIS_TESTS").as_deref() == Ok("1"),
    )
}

/// Returns a name that is unique within the current test run by combining `prefix`
/// with the output of [`uuidish`].  Used to isolate team and agent names across
/// concurrent test cases that share the same Redis instance.
fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuidish())
}

/// Generates a pseudo-unique identifier from the process ID and the current
/// wall-clock time in nanoseconds.  Not cryptographically random, but sufficient
/// to prevent key collisions between test runs and between concurrent test cases.
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

/// Opens a multiplexed async Redis connection to `url`.
///
/// # Panics
///
/// Panics if the client cannot be constructed or if the connection handshake fails.
/// Test helpers are allowed to panic on setup failures because the test itself would
/// be meaningless without a working connection.
async fn redis_connection(url: &str) -> redis::aio::MultiplexedConnection {
    redis::Client::open(url)
        .expect("redis client")
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection")
}

/// Returns `true` when `error` clearly communicates that a revoked or missing
/// invite token was the cause of the rejection.
///
/// This helper avoids asserting on a single error variant because the revocation
/// code path may surface the rejection either as a missing-lookup (`NotFound`)
/// when the lookup key has already been deleted, or as a validation failure
/// (`InvalidConfig`) when the invite hash record carries a `revoked_at` marker.
fn is_clear_revoked_rejection(error: MagiError) -> bool {
    match error {
        MagiError::NotFound(message) => message.contains("invite token"),
        MagiError::InvalidConfig(message) => {
            message.contains("revoked") || message.contains("invalid")
        }
        _ => false,
    }
}

// --- Unit tests for pure helpers (no Redis required) ---

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

/// Verifies that `parse_ttl` accepts the `h`, `m`, and `s` duration suffixes and
/// converts them to the correct number of seconds.
#[test]
fn ttl_parser_accepts_h_m_s_suffixes() {
    assert_eq!(parse_ttl("2h").expect("hours"), Duration::from_secs(7200));
    assert_eq!(parse_ttl("15m").expect("minutes"), Duration::from_secs(900));
    assert_eq!(parse_ttl("45s").expect("seconds"), Duration::from_secs(45));
}

/// Verifies that `parse_ttl` rejects empty strings, zero-duration values,
/// unsupported suffixes, and non-numeric input.
#[test]
fn ttl_parser_rejects_invalid_values() {
    assert!(parse_ttl("").is_err());
    assert!(parse_ttl("0s").is_err());
    assert!(parse_ttl("10d").is_err());
    assert!(parse_ttl("abc").is_err());
}

// --- Redis-backed integration tests ---

/// Happy-path test covering the complete invite lifecycle:
/// create team → create invite → join with invite → list members → revoke → reject late join.
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
    // Attempt to join with the now-revoked token — must be rejected.
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

/// Verifies that calling `register_agent_with_url` twice with identical arguments
/// is idempotent: the member list contains exactly one entry for the agent and the
/// registration set in Redis holds a single `"runtime:project"` value.
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

    // Verify at the Redis level that the SMEMBERS set holds exactly one entry.
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

/// Verifies the `revoked_at` marker path: the lookup key is left intact but the
/// invite hash carries `revoked_at`, which must cause `join_with_url` to return
/// `InvalidConfig` mentioning "revoked".
///
/// This is a white-box test that directly writes the `revoked_at` field into Redis
/// without going through `revoke_invite_with_url`, so it exercises the marker-check
/// branch independently of the lookup-deletion branch.
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

    // Inject the revoked_at marker directly, bypassing the normal revocation API,
    // to isolate the marker-check code path from lookup-key deletion.
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

/// Verifies that `revoke_invite_with_url` scopes revocation to the owning team:
/// attempting to revoke team A's invite via team B must fail with `NotFound` and
/// must leave both the lookup key and the invite hash (`revoked_at` absent) intact.
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

    // Confirm the lookup key still exists and no revoked_at marker was written.
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

/// Verifies that `list_invites_with_url` ignores invite IDs that appear in a
/// team's invite set but whose underlying invite hash belongs to a different team.
///
/// The test deliberately pollutes team B's invite-ID set with an ID that was
/// created under team A, then asserts that `list_invites_with_url` for team B
/// does not surface the foreign invite.
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

    // Inject team A's invite ID into team B's invite set to simulate a corrupted
    // or adversarially manipulated state.
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

/// Verifies that an invite whose Redis key has expired (TTL elapsed) is rejected
/// at join time.  A 1-second TTL is used and the test sleeps 1 200 ms to ensure
/// the key has expired before attempting to join.
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
    // Wait long enough for the 1-second Redis TTL to expire before attempting to join.
    tokio::time::sleep(Duration::from_millis(1200)).await;

    let error = join_with_url(&url, &invite.token, "agent", "codex", "/tmp/project")
        .await
        .expect_err("expired invite should fail");

    assert!(matches!(error, MagiError::NotFound(message) if message.contains("invite token")));
}

/// Verifies that an invite with `max_uses = 1` allows exactly one successful join
/// and rejects a subsequent join attempt with an `InvalidConfig` error mentioning
/// "maximum uses".
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

    // Write max_uses directly so this test does not depend on the invite-creation
    // API exposing a max_uses parameter.
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

/// Stress-tests the atomicity guarantee of the `max_uses = 1` check by firing
/// 24 concurrent `join_with_url` calls against the same single-use invite token.
///
/// The test asserts that exactly one call succeeds, all others receive
/// `InvalidConfig("maximum uses …")`, and the stored `used_count` is exactly 1.
/// This validates that the Redis atomic increment + compare prevents double-spending
/// under concurrent load.
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

    // Set max_uses = 1 directly in Redis.
    let keys = RedisKeys::new(&team);
    let mut connection = redis_connection(&url).await;
    let _: () = connection
        .hset(keys.invite(&invite.invite_id), "max_uses", 1)
        .await
        .expect("set max uses");

    // Spawn 24 concurrent join tasks to race for the single available use.
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

    // Collect results: exactly one success and 23 max-uses rejections are expected.
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

    // Verify counters and the Redis-level used_count field.
    let used_count: u64 = connection
        .hget(keys.invite(&invite.invite_id), "used_count")
        .await
        .expect("used count");
    assert_eq!(successes, 1);
    assert_eq!(failures, 23);
    assert_eq!(used_count, 1);
}

/// Verifies two security properties of the invite system:
///
/// 1. The raw token is never stored in Redis — only its hash (`token_hash`) is
///    written to the invite hash, and the `token` field is absent.
/// 2. Revoking an invite deletes the global lookup key so that the token can no
///    longer be resolved even if an attacker retains the plaintext token.
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

    // Read the invite hash directly to confirm the token field is absent.
    let stored_token: Option<String> = connection
        .hget(keys.invite(&invite.invite_id), "token")
        .await
        .expect("token field");
    let stored_hash: String = connection
        .hget(keys.invite(&invite.invite_id), "token_hash")
        .await
        .expect("token hash");
    // Read the global lookup key to confirm it resolves to the invite hash key.
    let lookup_team_and_id: Option<String> = connection.get(&lookup_key).await.expect("lookup");

    assert_eq!(stored_token, None);
    assert_eq!(stored_hash, hash);
    assert_eq!(lookup_team_and_id, Some(keys.invite(&invite.invite_id)));

    // After revocation the lookup key must be deleted so the token becomes unusable.
    revoke_invite_with_url(&url, &team, &invite.invite_id)
        .await
        .unwrap();
    let exists_after_revoke: bool = connection.exists(&lookup_key).await.expect("exists");
    assert!(!exists_after_revoke);
}
