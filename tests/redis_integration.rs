use magi::error::MagiError;
use magi::model::{MessageEvent, RedisKeys, REDIS_KEY_PREFIX};

#[test]
fn redis_keys_build_stable_names() {
    let keys = RedisKeys::new("team-alpha");

    assert_eq!(REDIS_KEY_PREFIX, "magi");
    assert_eq!(keys.teams(), "magi:teams");
    assert_eq!(keys.team(), "magi:team:team-alpha");
    assert_eq!(keys.team_agents(), "magi:team:team-alpha:agents");
    assert_eq!(keys.agent("agent-01"), "magi:agent:team-alpha:agent-01");
    assert_eq!(
        keys.registrations("agent-01"),
        "magi:agent:team-alpha:agent-01:registrations"
    );
    assert_eq!(keys.stream(), "magi:stream:team-alpha");
    assert_eq!(keys.cursor("agent-01"), "magi:cursor:team-alpha:agent-01");
    assert_eq!(keys.pubsub(), "magi:pubsub:team-alpha");
    assert_eq!(keys.invite("invite-01"), "magi:invite:invite-01");
    assert_eq!(
        RedisKeys::invite_token("token-hash-01"),
        "magi:invite_token:token-hash-01"
    );
}

#[test]
fn redis_keys_preserve_dash_and_underscore_ids() {
    let keys = RedisKeys::new("team_alpha-beta");

    assert_eq!(
        keys.agent("agent_beta-01"),
        "magi:agent:team_alpha-beta:agent_beta-01"
    );
    assert_eq!(
        keys.registrations("agent_beta-01"),
        "magi:agent:team_alpha-beta:agent_beta-01:registrations"
    );
    assert_eq!(
        keys.cursor("agent_beta-01"),
        "magi:cursor:team_alpha-beta:agent_beta-01"
    );
}

#[test]
fn redis_keys_percent_encode_colon_segments() {
    let keys = RedisKeys::new("team:agents");

    assert_eq!(keys.team(), "magi:team:team%3Aagents");
    assert_eq!(keys.team_agents(), "magi:team:team%3Aagents:agents");
    assert_eq!(
        keys.agent("agent:01"),
        "magi:agent:team%3Aagents:agent%3A01"
    );
    assert_eq!(
        keys.registrations("agent:01"),
        "magi:agent:team%3Aagents:agent%3A01:registrations"
    );
    assert_eq!(
        keys.cursor("agent:01"),
        "magi:cursor:team%3Aagents:agent%3A01"
    );
    assert_eq!(keys.invite("invite:01"), "magi:invite:invite%3A01");
    assert_eq!(
        RedisKeys::invite_token("token:hash"),
        "magi:invite_token:token%3Ahash"
    );
}

#[test]
fn redis_keys_colon_ids_do_not_collide_with_normal_key_shapes() {
    let encoded_team = RedisKeys::new("team:agents");
    let normal_team = RedisKeys::new("team");

    assert_ne!(encoded_team.team(), normal_team.team_agents());
    assert_ne!(
        encoded_team.agent("agent:01"),
        RedisKeys::new("team:agents").agent("agent")
    );
    assert!(!encoded_team.team().contains("team:agents"));
    assert!(!encoded_team.agent("agent:01").contains("agent:01"));
}

#[test]
fn message_event_supports_string_timestamp_equality_and_toml_roundtrip() {
    let event = MessageEvent {
        from: "alice".to_string(),
        to: "bob".to_string(),
        body: "hello".to_string(),
        created_at: "2026-05-30T00:00:00Z".to_string(),
    };

    let same_event = MessageEvent {
        from: "alice".to_string(),
        to: "bob".to_string(),
        body: "hello".to_string(),
        created_at: "2026-05-30T00:00:00Z".to_string(),
    };

    assert_eq!(event, same_event);

    let encoded = toml::to_string(&event).unwrap();
    assert!(encoded.contains(r#"created_at = "2026-05-30T00:00:00Z""#));

    let decoded: MessageEvent = toml::from_str(&encoded).unwrap();
    assert_eq!(decoded, event);
}

#[test]
fn magi_error_display_includes_underlying_error() {
    let error = MagiError::Io(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "permission denied",
    ));

    assert_eq!(error.to_string(), "io error: permission denied");
}

#[tokio::test]
async fn connect_rejects_invalid_url() {
    let error = magi::redis_client::connect("not-a-redis-url").await;

    assert!(error.is_err());
}

#[tokio::test]
async fn ping_rejects_unreachable_local_port() {
    let error = magi::redis_client::ping("redis://127.0.0.1:1").await;

    assert!(error.is_err());
}

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

#[tokio::test]
async fn ping_configured_redis() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };

    magi::redis_client::ping(&url).await.unwrap();
}

#[tokio::test]
async fn publish_succeeds_without_subscribers() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };

    magi::redis_client::publish(&url, "magi:test:publish", "hello")
        .await
        .unwrap();
}
