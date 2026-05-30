//! Watch mode: streaming output of incoming messages.
//!
//! This module implements the `magi watch` command, which keeps a long-running
//! connection to Redis and prints every new message addressed to the active agent
//! as it arrives.  Output can be formatted either as a compact human-readable
//! line (`WatchFormat::Line`) or as machine-parseable NDJSON (`WatchFormat::Json`).
//!
//! ## How it works
//!
//! 1. A Pub/Sub subscription is opened on the team-scoped Redis channel
//!    (see `RedisKeys::pubsub`).  Senders publish a wakeup notification to this
//!    channel each time a new message is appended to the shared Stream.
//! 2. A 5-second fallback ticker ensures that messages are never missed even if
//!    a wakeup notification is dropped (e.g. due to a transient network hiccup).
//! 3. On each wakeup (or tick), `messaging::read_inbox_with_url` drains all
//!    unread messages from the agent's inbox slice of the Redis Stream and marks
//!    them as consumed.
//! 4. `Ctrl-C` (SIGINT) causes a clean exit.

use futures_util::StreamExt;
use serde_json::json;

use crate::cli::WatchFormat;
use crate::config::AppConfig;
use crate::error::{MagiError, Result};
use crate::messaging::{self, InboxReadMode, MessageRecord};
use crate::model::RedisKeys;

/// Entry point for the `magi watch` command.
///
/// Loads the application configuration from disk, resolves the Redis URL and
/// the active team/agent identity, then delegates to `watch_loop_with_url` to
/// run the event loop.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` if `redis.url`, `identity.active_team`,
/// or `identity.active_agent` are absent from the configuration file.
/// Propagates any Redis connectivity errors returned by the event loop.
pub async fn run(format: WatchFormat) -> Result<()> {
    let config = AppConfig::load()?;
    let url = config
        .redis
        .url
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("redis.url is not configured".to_string()))?;
    let team =
        config.identity.active_team.clone().ok_or_else(|| {
            MagiError::InvalidConfig("identity.active_team is required".to_string())
        })?;
    let agent =
        config.identity.active_agent.clone().ok_or_else(|| {
            MagiError::InvalidConfig("identity.active_agent is required".to_string())
        })?;

    watch_loop_with_url(&url, &team, &agent, format).await
}

/// Reads and formats all currently unread messages for the agent in one shot.
///
/// Unlike the long-running event loop this function opens a single connection,
/// drains the inbox with `InboxReadMode::MarkRead` (so each message is consumed
/// exactly once), serializes every record according to `format`, and returns.
///
/// This is used both by the event loop on each wakeup and can be called directly
/// from tests or scripts that only need a single poll.
///
/// # Parameters
///
/// - `url`    — Redis connection URL (e.g. `redis://127.0.0.1:6379`).
/// - `team`   — Team name; used to derive the Redis Stream and Pub/Sub key.
/// - `agent`  — Agent name; used to identify the consumer group / inbox slice.
/// - `format` — Desired output format (`Line` or `Json`).
///
/// # Errors
///
/// Propagates Redis connection or command errors.  Also returns an error if
/// serialization fails (unlikely for the supported formats).
pub async fn watch_once_with_url(
    url: &str,
    team: &str,
    agent: &str,
    format: WatchFormat,
) -> Result<Vec<String>> {
    let messages =
        messaging::read_inbox_with_url(url, team, agent, InboxReadMode::MarkRead).await?;
    messages
        .iter()
        .map(|message| format_watch_message(message, format))
        .collect()
}

/// Serializes a single `MessageRecord` into the requested output format.
///
/// - `WatchFormat::Line` — Delegates to `messaging::format_message_line` which
///   produces a compact, human-readable one-liner (e.g. `[id] from → to: body`).
/// - `WatchFormat::Json` — Produces a flat JSON object with the keys `id`,
///   `from`, `to`, `body`, and `created_at`, suitable for NDJSON piping.
///
/// # Errors
///
/// This function is currently infallible for both variants, but returns
/// `Result<String>` so that future formats can signal serialization failures
/// without a breaking API change.
pub fn format_watch_message(message: &MessageRecord, format: WatchFormat) -> Result<String> {
    match format {
        WatchFormat::Line => Ok(messaging::format_message_line(message)),
        WatchFormat::Json => Ok(json!({
            "id": message.id,
            "from": message.event.from,
            "to": message.event.to,
            "body": message.event.body,
            "created_at": message.event.created_at,
        })
        .to_string()),
    }
}

/// Long-running event loop that streams messages to stdout until interrupted.
///
/// Opens a Redis Pub/Sub connection on the team channel (`RedisKeys::pubsub`)
/// and drives a `tokio::select!` loop with three branches:
///
/// - **Ctrl-C**: Exits cleanly with `Ok(())`.
/// - **5-second ticker**: Polls for new messages periodically as a safety net
///   in case a Pub/Sub wakeup notification was dropped.
/// - **Pub/Sub wakeup**: Polls for new messages immediately when a sender
///   notifies the channel, giving near-real-time delivery.
///
/// On each poll, `print_new_messages` drains the inbox and prints each message
/// line to stdout.
///
/// # Errors
///
/// Returns any error encountered while connecting to Redis or while reading
/// and printing messages.
async fn watch_loop_with_url(
    url: &str,
    team: &str,
    agent: &str,
    format: WatchFormat,
) -> Result<()> {
    let client = redis::Client::open(url)?;
    // Open a dedicated async Pub/Sub connection; this is separate from the
    // connection used by read_inbox_with_url so the two do not interfere.
    let mut pubsub = client.get_async_pubsub().await?;
    // Subscribe to the team-scoped Pub/Sub channel.  Senders publish here
    // after appending a message to the shared Redis Stream.
    pubsub.subscribe(RedisKeys::new(team).pubsub()).await?;
    // Wrap the subscription in a Stream so we can use .next() in select!.
    let mut wakeups = pubsub.on_message();
    // Fallback ticker: poll every 5 s even if wakeup notifications are missed.
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));

    loop {
        tokio::select! {
            // Graceful shutdown on Ctrl-C / SIGINT.
            _ = tokio::signal::ctrl_c() => return Ok(()),
            // Periodic safety poll to handle dropped wakeup notifications.
            _ = interval.tick() => {
                print_new_messages(url, team, agent, format).await?;
            }
            // Immediate poll triggered by a Pub/Sub wakeup from a sender.
            Some(_) = wakeups.next() => {
                print_new_messages(url, team, agent, format).await?;
            }
        }
    }
}

/// Reads all unread messages and prints each formatted line to stdout.
///
/// Delegates to `watch_once_with_url` (which marks messages as read) and then
/// iterates over the resulting formatted strings, writing one per line.
///
/// # Errors
///
/// Propagates errors from `watch_once_with_url`.
async fn print_new_messages(url: &str, team: &str, agent: &str, format: WatchFormat) -> Result<()> {
    for line in watch_once_with_url(url, team, agent, format).await? {
        println!("{line}");
    }
    Ok(())
}
