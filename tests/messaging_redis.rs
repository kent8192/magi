use futures_util::StreamExt;
use magi::error::MagiError;
use magi::messaging::{
    history_with_url, read_inbox_with_url, send_message_with_url, InboxReadMode,
};
use magi::model::RedisKeys;
use magi::team::{create_team_with_url, register_agent_with_url};
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
async fn send_appends_stream_event_and_publishes_wakeup() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-msg-send");
    let alice = unique_name("alice");
    let bob = unique_name("bob");

    create_team_with_url(&url, &team, &alice).await.unwrap();
    register_agent_with_url(&url, &team, &bob, "codex", "/tmp/bob")
        .await
        .unwrap();

    let mut subscriber = redis::Client::open(url.as_str())
        .unwrap()
        .get_async_pubsub()
        .await
        .unwrap();
    subscriber
        .subscribe(RedisKeys::new(&team).pubsub())
        .await
        .unwrap();

    let message = send_message_with_url(&url, &team, &alice, &bob, "deploy is done")
        .await
        .unwrap();

    assert_eq!(message.event.from, alice);
    assert_eq!(message.event.to, bob);
    assert_eq!(message.event.body, "deploy is done");
    assert!(!message.id.is_empty());
    assert!(!message.event.created_at.is_empty());

    let mut connection = redis_connection(&url).await;
    let stream_len: usize = connection
        .xlen(RedisKeys::new(&team).stream())
        .await
        .unwrap();
    assert_eq!(stream_len, 1);

    let mut messages = subscriber.on_message();
    let published = tokio::time::timeout(std::time::Duration::from_secs(2), messages.next())
        .await
        .expect("pubsub wakeup")
        .expect("published message");
    let payload: String = published.get_payload().unwrap();
    assert_eq!(payload, message.id);
}

#[tokio::test]
async fn send_rejects_unknown_recipient_without_writing_stream() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-msg-missing-to");
    let alice = unique_name("alice");

    create_team_with_url(&url, &team, &alice).await.unwrap();

    let error = send_message_with_url(&url, &team, &alice, "missing", "hello")
        .await
        .expect_err("unknown recipient must fail");
    assert!(matches!(error, MagiError::NotFound(message) if message.contains("recipient")));

    let mut connection = redis_connection(&url).await;
    let exists: bool = connection
        .exists(RedisKeys::new(&team).stream())
        .await
        .unwrap();
    assert!(!exists);
}

#[tokio::test]
async fn send_rejects_blank_message_body() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let error = send_message_with_url(&url, "team", "alice", "bob", "   ")
        .await
        .expect_err("blank message should fail");

    assert!(matches!(error, MagiError::InvalidConfig(message) if message.contains("message body")));
}

#[tokio::test]
async fn inbox_returns_target_messages_once_and_advances_cursor() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-msg-inbox");
    let alice = unique_name("alice");
    let bob = unique_name("bob");
    let carol = unique_name("carol");

    create_team_with_url(&url, &team, &alice).await.unwrap();
    register_agent_with_url(&url, &team, &bob, "codex", "/tmp/bob")
        .await
        .unwrap();
    register_agent_with_url(&url, &team, &carol, "codex", "/tmp/carol")
        .await
        .unwrap();

    send_message_with_url(&url, &team, &alice, &carol, "not for bob")
        .await
        .unwrap();
    let expected = send_message_with_url(&url, &team, &alice, &bob, "for bob")
        .await
        .unwrap();

    let messages = read_inbox_with_url(&url, &team, &bob, InboxReadMode::MarkRead)
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].id, expected.id);
    assert_eq!(messages[0].event.body, "for bob");

    let second = read_inbox_with_url(&url, &team, &bob, InboxReadMode::MarkRead)
        .await
        .unwrap();
    assert!(second.is_empty());

    let mut connection = redis_connection(&url).await;
    let cursor: String = connection
        .get(RedisKeys::new(&team).cursor(&bob))
        .await
        .unwrap();
    assert_eq!(cursor, expected.id);
}

#[tokio::test]
async fn inbox_peek_does_not_advance_cursor() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-msg-peek");
    let alice = unique_name("alice");
    let bob = unique_name("bob");

    create_team_with_url(&url, &team, &alice).await.unwrap();
    register_agent_with_url(&url, &team, &bob, "codex", "/tmp/bob")
        .await
        .unwrap();
    send_message_with_url(&url, &team, &alice, &bob, "peek me")
        .await
        .unwrap();

    let peek = read_inbox_with_url(&url, &team, &bob, InboxReadMode::Peek)
        .await
        .unwrap();
    let after_peek = read_inbox_with_url(&url, &team, &bob, InboxReadMode::MarkRead)
        .await
        .unwrap();

    assert_eq!(peek.len(), 1);
    assert_eq!(after_peek.len(), 1);
    assert_eq!(peek[0].id, after_peek[0].id);
}

#[tokio::test]
async fn inbox_advances_over_non_target_messages_without_returning_them() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-msg-skip");
    let alice = unique_name("alice");
    let bob = unique_name("bob");
    let carol = unique_name("carol");

    create_team_with_url(&url, &team, &alice).await.unwrap();
    register_agent_with_url(&url, &team, &bob, "codex", "/tmp/bob")
        .await
        .unwrap();
    register_agent_with_url(&url, &team, &carol, "codex", "/tmp/carol")
        .await
        .unwrap();
    let skipped = send_message_with_url(&url, &team, &alice, &carol, "skip")
        .await
        .unwrap();

    let messages = read_inbox_with_url(&url, &team, &bob, InboxReadMode::MarkRead)
        .await
        .unwrap();
    assert!(messages.is_empty());

    let mut connection = redis_connection(&url).await;
    let cursor: String = connection
        .get(RedisKeys::new(&team).cursor(&bob))
        .await
        .unwrap();
    assert_eq!(cursor, skipped.id);
}

#[tokio::test]
async fn history_can_return_all_messages_or_filter_by_agent() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-msg-history");
    let alice = unique_name("alice");
    let bob = unique_name("bob");
    let carol = unique_name("carol");

    create_team_with_url(&url, &team, &alice).await.unwrap();
    register_agent_with_url(&url, &team, &bob, "codex", "/tmp/bob")
        .await
        .unwrap();
    register_agent_with_url(&url, &team, &carol, "codex", "/tmp/carol")
        .await
        .unwrap();

    send_message_with_url(&url, &team, &alice, &bob, "one")
        .await
        .unwrap();
    send_message_with_url(&url, &team, &bob, &carol, "two")
        .await
        .unwrap();

    let all = history_with_url(&url, &team, None).await.unwrap();
    let bob_only = history_with_url(&url, &team, Some(&bob)).await.unwrap();

    assert_eq!(all.len(), 2);
    assert_eq!(bob_only.len(), 2);
    assert!(bob_only
        .iter()
        .all(|message| message.event.from == bob || message.event.to == bob));
}

#[tokio::test]
async fn history_missing_stream_returns_empty_list() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-msg-empty-history");

    let messages = history_with_url(&url, &team, None).await.unwrap();

    assert!(messages.is_empty());
}
