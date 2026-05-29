use magi::cli::WatchFormat;
use magi::messaging::send_message_with_url;
use magi::repl::{parse_repl_command, ReplCommand};
use magi::team::{create_team_with_url, register_agent_with_url};
use magi::watch::{format_watch_message, watch_once_with_url};

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

#[test]
fn repl_parser_maps_empty_input_to_inbox() {
    assert_eq!(parse_repl_command("").unwrap(), ReplCommand::Inbox);
    assert_eq!(parse_repl_command("   ").unwrap(), ReplCommand::Inbox);
}

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

#[test]
fn repl_parser_rejects_incomplete_or_unknown_commands() {
    assert!(parse_repl_command("send bob").is_err());
    assert!(parse_repl_command("send").is_err());
    assert!(parse_repl_command("history alice extra").is_err());
    assert!(parse_repl_command("wat").is_err());
}

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

#[tokio::test]
async fn watch_once_returns_new_messages_and_marks_them_read() {
    let Some(url) = redis_url_from_env() else {
        eprintln!("skipping Redis-backed test; MAGI_TEST_REDIS_URL is not set");
        return;
    };
    let team = unique_name("team-watch-once");
    let alice = unique_name("alice");
    let bob = unique_name("bob");

    create_team_with_url(&url, &team, &alice).await.unwrap();
    register_agent_with_url(&url, &team, &bob, "codex", "/tmp/bob")
        .await
        .unwrap();
    send_message_with_url(&url, &team, &alice, &bob, "watch me")
        .await
        .unwrap();

    let first = watch_once_with_url(&url, &team, &bob, WatchFormat::Line)
        .await
        .unwrap();
    let second = watch_once_with_url(&url, &team, &bob, WatchFormat::Line)
        .await
        .unwrap();

    assert_eq!(first.len(), 1);
    assert!(first[0].contains("watch me"));
    assert!(second.is_empty());
}
