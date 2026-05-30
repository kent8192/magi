//! Integration tests for Redis key construction, model serialization, error formatting,
//! and live Redis connectivity in the `magi` crate.
//!
//! # Test categories
//!
//! - **Key naming** (`redis_keys_*`): pure unit tests that verify every `RedisKeys` accessor
//!   produces the expected `magi:<segment>:<id>` string, including percent-encoding of colons
//!   and preservation of dashes and underscores.
//! - **Model** (`message_event_*`): verifies `MessageEvent` equality and TOML round-trip.
//! - **Error** (`magi_error_*`): checks `MagiError` `Display` formatting.
//! - **Connectivity** (async): tests that require a real Redis instance take the
//!   `redis_fixture` rstest fixture, which provisions an ephemeral Redis container
//!   via testcontainers (Docker required).

use magi::error::MagiError;
use magi::model::{MessageEvent, RedisKeys, REDIS_KEY_PREFIX};

mod common;
use common::{redis_fixture, RedisFixture};
use rstest::rstest;

/// Verifies that every `RedisKeys` accessor returns its canonical `magi:<segment>` form and
/// that `REDIS_KEY_PREFIX` is exactly `"magi"`.
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

/// Dashes and underscores in team/agent IDs must pass through unchanged (no encoding needed).
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

/// Colons inside IDs must be percent-encoded as `%3A` so they cannot be confused with the
/// colon delimiter that separates key segments (e.g. `magi:team:<id>`).
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

/// Encoding must be injective: a team named `"team:agents"` must produce keys that are
/// distinct from the keys of a team named `"team"` with a sub-key `"agents"`.
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

/// `MessageEvent` is stored/transmitted as TOML.  This test checks structural equality
/// and that `created_at` survives a `toml::to_string` / `toml::from_str` round-trip intact.
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

/// Verifies that `ping` succeeds against a real Redis server.
#[rstest]
#[tokio::test]
async fn ping_configured_redis(#[future(awt)] redis_fixture: RedisFixture) {
    let url = redis_fixture.url().to_string();

    magi::redis_client::ping(&url).await.unwrap();
}

/// Verifies that `publish` on a Pub/Sub channel succeeds even when no subscriber is present.
/// Redis `PUBLISH` returns the number of receivers (zero here), which should not be an error.
#[rstest]
#[tokio::test]
async fn publish_succeeds_without_subscribers(#[future(awt)] redis_fixture: RedisFixture) {
    let url = redis_fixture.url().to_string();

    magi::redis_client::publish(&url, "magi:test:publish", "hello")
        .await
        .unwrap();
}
