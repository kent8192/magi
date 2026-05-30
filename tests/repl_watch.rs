//! Integration and unit tests for the magi REPL command parser and watch-mode output formatter.
//!
//! # Test categories
//!
//! - **REPL parser (pure, no I/O)** — verifies that `parse_repl_command` correctly maps user
//!   input strings to `ReplCommand` variants and rejects malformed or unknown commands.
//! - **Watch formatter (pure, no I/O)** — verifies that `format_watch_message` produces the
//!   expected `line` and `json` representations for a `MessageRecord`.
//! - **Watch once (Redis-backed)** — verifies end-to-end that `watch_once_with_url` reads
//!   new messages from a Redis Stream and advances the consumer position so a second call
//!   returns an empty list (at-most-once delivery semantics).
//!
//! # Redis-backed tests
//!
//! Tests that require a live Redis server are skipped by default.  Set the environment variable
//! `MAGI_TEST_REDIS_URL` to a valid Redis URL (e.g. `redis://127.0.0.1:6379`) to enable them.
//! If `MAGI_REQUIRE_REDIS_TESTS=1` is also set, the test binary will panic rather than silently
//! skip when `MAGI_TEST_REDIS_URL` is absent, making it suitable for CI enforcement.

use magi::cli::WatchFormat;
use magi::messaging::send_message_with_url;
use magi::repl::{parse_repl_command, ReplCommand};
use magi::team::{create_team_with_url, register_agent_with_url};
use magi::watch::{format_watch_message, watch_once_with_url};

/// Resolves a Redis URL from explicitly supplied values, enforcing the CI hard-fail gate.
///
/// Returns `None` when `url` is absent or blank and `require_redis_tests` is `false`,
/// allowing callers to skip Redis-backed tests gracefully.
///
/// # Panics
///
/// Panics when `url` is `None` (or blank) and `require_redis_tests` is `true`, so that CI
/// jobs which set `MAGI_REQUIRE_REDIS_TESTS=1` fail loudly instead of silently skipping.
fn redis_url_from_values(url: Option<String>, require_redis_tests: bool) -> Option<String> {
    let url = url.filter(|url| !url.trim().is_empty());

    if url.is_none() && require_redis_tests {
        panic!("MAGI_REQUIRE_REDIS_TESTS=1 requires MAGI_TEST_REDIS_URL to be set");
    }

    url
}

/// Reads the Redis URL and the hard-fail gate flag from environment variables.
///
/// Returns the value of `MAGI_TEST_REDIS_URL` when it is non-empty, or `None` when the
/// variable is unset/blank.  If `MAGI_REQUIRE_REDIS_TESTS=1` is set and the URL is absent
/// the call delegates to `redis_url_from_values`, which panics.
fn redis_url_from_env() -> Option<String> {
    redis_url_from_values(
        std::env::var("MAGI_TEST_REDIS_URL").ok(),
        std::env::var("MAGI_REQUIRE_REDIS_TESTS").as_deref() == Ok("1"),
    )
}

/// Creates a unique name by appending a pseudo-UUID to `prefix`.
///
/// Used to generate isolated team and agent names so parallel test runs do not collide in
/// a shared Redis instance.
fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}", uuidish())
}

/// Generates a pseudo-unique identifier from the current process ID and wall-clock nanoseconds.
///
/// This is not cryptographically random but is unique enough for test isolation purposes.
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

/// Blank and whitespace-only input should default to the `Inbox` command.
#[test]
fn repl_parser_maps_empty_input_to_inbox() {
    assert_eq!(parse_repl_command("").unwrap(), ReplCommand::Inbox);
    assert_eq!(parse_repl_command("   ").unwrap(), ReplCommand::Inbox);
}

/// All documented REPL verbs — `send`, `inbox`, `team`, `history`, `quit`, `exit` — parse
/// to their corresponding `ReplCommand` variants with the correct fields.
#[test]
fn repl_parser_accepts_expected_commands() {
    assert_eq!(
        parse_repl_command("send bob deploy is done").unwrap(),
        ReplCommand::Send {
            to: "bob".to_string(),
            body: "deploy is done".to_string()
        }
    );
    assert_eq!(parse_repl_command("inbox").unwrap(), ReplCommand::Inbox);
    assert_eq!(parse_repl_command("team").unwrap(), ReplCommand::Team);
    assert_eq!(
        parse_repl_command("history alice").unwrap(),
        ReplCommand::History {
            agent: Some("alice".to_string())
        }
    );
    assert_eq!(
        parse_repl_command("history").unwrap(),
        ReplCommand::History { agent: None }
    );
    assert_eq!(parse_repl_command("quit").unwrap(), ReplCommand::Quit);
    assert_eq!(parse_repl_command("exit").unwrap(), ReplCommand::Quit);
}

/// `send` without a body, `send` without both recipient and body, `history` with extra tokens,
/// and completely unknown verbs should all return an `Err`.
#[test]
fn repl_parser_rejects_incomplete_or_unknown_commands() {
    assert!(parse_repl_command("send bob").is_err());
    assert!(parse_repl_command("send").is_err());
    assert!(parse_repl_command("history alice extra").is_err());
    assert!(parse_repl_command("wat").is_err());
}

/// `WatchFormat::Line` should render as `[<created_at>] <from> -> <to>: <body>`.
#[test]
fn watch_formats_line_messages() {
    let message = magi::messaging::MessageRecord {
        id: "1-0".to_string(),
        event: magi::model::MessageEvent {
            from: "alice".to_string(),
            to: "bob".to_string(),
            body: "deploy is done".to_string(),
            created_at: "123".to_string(),
        },
    };

    assert_eq!(
        format_watch_message(&message, WatchFormat::Line).unwrap(),
        "[123] alice -> bob: deploy is done"
    );
}

/// `WatchFormat::Json` should produce valid JSON with all fields present and special characters
/// (double-quotes, newlines) properly escaped so that round-tripping through `serde_json` is
/// lossless.
#[test]
fn watch_formats_json_messages_with_escaping() {
    let message = magi::messaging::MessageRecord {
        id: "1-0".to_string(),
        event: magi::model::MessageEvent {
            from: "alice".to_string(),
            to: "bob".to_string(),
            body: "quote \" and newline\n".to_string(),
            created_at: "123".to_string(),
        },
    };

    let formatted = format_watch_message(&message, WatchFormat::Json).unwrap();
    let decoded: serde_json::Value = serde_json::from_str(&formatted).unwrap();

    assert_eq!(decoded["id"], "1-0");
    assert_eq!(decoded["from"], "alice");
    assert_eq!(decoded["to"], "bob");
    assert_eq!(decoded["body"], "quote \" and newline\n");
}

/// Verifies the at-most-once read semantics of `watch_once_with_url`.
///
/// A message sent by `alice` to `bob` appears exactly once in the first `watch_once` call;
/// the second call returns an empty list because the Redis Stream consumer position has been
/// advanced past the message.
///
/// This test is skipped when `MAGI_TEST_REDIS_URL` is not set.  Set `MAGI_REQUIRE_REDIS_TESTS=1`
/// to promote the skip to a hard failure in CI.
#[tokio::test]
async fn watch_once_returns_new_messages_and_marks_them_read() {
    // Skip gracefully when no Redis URL is configured; fail loudly when MAGI_REQUIRE_REDIS_TESTS=1.
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    // Use unique names so concurrent test runs on a shared Redis do not interfere.
    let team = unique_name("team-watch-once");
    let alice = unique_name("alice");
    let bob = unique_name("bob");

    // Bootstrap: create the team with alice as founder, then register bob as a second member.
    create_team_with_url(&url, &team, &alice).await.unwrap();
    register_agent_with_url(&url, &team, &bob, "codex", "/tmp/bob")
        .await
        .unwrap();
    // Publish exactly one message to bob's inbox via the Redis Stream.
    send_message_with_url(&url, &team, &alice, &bob, "watch me")
        .await
        .unwrap();

    // First poll: should return the one pending message and advance the stream consumer position.
    let first = watch_once_with_url(&url, &team, &bob, WatchFormat::Line)
        .await
        .unwrap();
    // Second poll: consumer position is already past the message, so the result must be empty.
    let second = watch_once_with_url(&url, &team, &bob, WatchFormat::Line)
        .await
        .unwrap();

    assert_eq!(first.len(), 1);
    assert!(first[0].contains("watch me"));
    assert!(second.is_empty());
}
