//! Cross-agent messaging backed by Redis Streams and Pub/Sub.
//!
//! This module implements the core message transport for the `magi` CLI:
//! sending a message to another agent, draining an agent's inbox, and
//! browsing the full team history. Every message lives in a per-team Redis
//! Stream and is announced over a per-team Pub/Sub channel so that live
//! consumers (for example the watch mode and the REPL) can react to new
//! traffic without polling.
//!
//! ## Data layout in Redis
//!
//! All keys are derived from the active team via `RedisKeys`:
//!
//! ```text
//! <team> stream   : append-only log of messages (XADD / XRANGE)
//! <team> pubsub   : channel notified with each new stream entry id
//! <team> agents   : set of agent names known to the team
//! <team> cursor   : per-agent "last read" stream id for the inbox
//! ```
//!
//! The stream is the source of truth; Pub/Sub is a best-effort
//! notification side channel only. Each agent tracks how far it has read
//! through its own cursor key, which makes the inbox a destructive
//! ("mark as read") read by default while history remains non-destructive.
//!
//! ## Public surface
//!
//! The `*_with_url` functions take an explicit Redis URL and the resolved
//! team/agent names, which keeps them easy to test against an arbitrary
//! Redis instance. The thin `send`, `inbox`, and `history` wrappers
//! load the on-disk `AppConfig`, resolve the active identity, and print
//! human-readable output for the CLI subcommands of the same name.

use redis::AsyncCommands;

use crate::config::AppConfig;
use crate::error::{MagiError, Result};
use crate::model::{MessageEvent, RedisKeys};
use crate::redis_client;
use crate::team;

/// Controls whether reading the inbox advances the per-agent read cursor.
///
/// `MarkRead` is the normal inbox behavior (consume and acknowledge),
/// while `Peek` lets callers preview pending messages without mutating
/// the cursor — useful for watch/preview flows that must not consume.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InboxReadMode {
    /// Advance the agent's cursor to the last seen stream id, so the same
    /// messages are not returned again on the next inbox read.
    MarkRead,
    /// Leave the cursor untouched; messages remain "unread" afterwards.
    Peek,
}

/// A single message as stored in (and read back from) the Redis Stream.
///
/// Pairs the stream entry id assigned by Redis with the decoded
/// `MessageEvent` payload (sender, recipient, body, timestamp).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MessageRecord {
    /// Redis Stream entry id (for example `1700000000000-0`). Stream ids
    /// are monotonically increasing, which is what makes the inbox cursor
    /// and the `(id` exclusive range scan below well defined.
    pub id: String,
    /// Decoded message payload (from / to / body / created_at).
    pub event: MessageEvent,
}

/// CLI entry point for `magi send`: deliver a message to another agent.
///
/// Loads the persisted `AppConfig`, resolves the active team and the
/// active agent (used as the sender), joins the `message` words into a
/// single space-separated body, and appends it to the team stream. On
/// success a confirmation line `sent <id> <from> -> <to>` is printed.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded, if `redis.url`,
/// `identity.active_team`, or `identity.active_agent` are unset, if either
/// the sender or recipient is not a registered team agent, or if the body
/// is empty (see `send_message_with_url`).
pub async fn send(to: String, message: Vec<String>) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let team = active_team(&config)?;
    // The active agent is always the sender for the interactive `send` command.
    let from = active_agent(&config)?;
    // Join the variadic CLI words back into a single message body.
    let body = message.join(" ");

    let record = send_message_with_url(&url, &team, &from, &to, &body).await?;
    println!(
        "sent {} {} -> {}",
        record.id, record.event.from, record.event.to
    );
    Ok(())
}

/// CLI entry point for `magi inbox`: print and consume unread messages.
///
/// Reads every message addressed to the active agent that arrived after
/// its last read cursor, prints each on one line, and advances the cursor
/// so the next call only returns newer traffic (`InboxReadMode::MarkRead`).
///
/// # Errors
///
/// Returns an error if the config or active identity cannot be resolved,
/// or if the underlying Redis read fails (see `read_inbox_with_url`).
pub async fn inbox() -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let team = active_team(&config)?;
    let agent = active_agent(&config)?;

    // MarkRead advances this agent's cursor, so consumed messages are not
    // shown again on the next `inbox` invocation.
    for message in read_inbox_with_url(&url, &team, &agent, InboxReadMode::MarkRead).await? {
        println!("{}", format_message_line(&message));
    }
    Ok(())
}

/// CLI entry point for `magi history`: print the full team message log.
///
/// Unlike `inbox`, this is a non-destructive read of the entire stream
/// and never touches any cursor. The `team` argument overrides the active
/// team when provided, otherwise the config's `identity.active_team` is
/// used. When `agent` is `Some`, the log is filtered to messages that
/// agent either sent or received.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded, if no team can be
/// resolved (neither argument nor active team set), or if the Redis scan
/// fails (see `history_with_url`).
pub async fn history(team: Option<String>, agent: Option<String>) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    // Prefer the explicit team argument, falling back to the active team.
    let team = team
        .or(config.identity.active_team)
        .ok_or_else(|| MagiError::InvalidConfig("identity.active_team is required".to_string()))?;

    for message in history_with_url(&url, &team, agent.as_deref()).await? {
        println!("{}", format_message_line(&message));
    }
    Ok(())
}

/// Append a message to the team stream and announce it over Pub/Sub.
///
/// This is the testable core behind `send`. It validates the body,
/// verifies both endpoints are registered team agents, writes the message
/// to the Redis Stream with `XADD`, and publishes the new entry id to the
/// team's Pub/Sub channel for live consumers.
///
/// # Parameters
///
/// - `url`: Redis connection URL.
/// - `team`: team name used to derive all Redis keys.
/// - `from` / `to`: sender and recipient agent names (both must exist).
/// - `body`: message text; surrounding whitespace is trimmed.
///
/// # Returns
///
/// A `MessageRecord` containing the Redis-assigned stream `id` and the
/// stored `MessageEvent`.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` if the trimmed body is empty,
/// `MagiError::NotFound` if the sender or recipient is not a team agent,
/// or any Redis error from connecting, `XADD`, or `PUBLISH`.
pub async fn send_message_with_url(
    url: &str,
    team: &str,
    from: &str,
    to: &str,
    body: &str,
) -> Result<MessageRecord> {
    // Reject whitespace-only messages early, before touching Redis.
    let body = body.trim();
    if body.is_empty() {
        return Err(MagiError::InvalidConfig(
            "message body must not be empty".to_string(),
        ));
    }

    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    // Both endpoints must be members of the team's agent set; otherwise the
    // message could be undeliverable or address a non-existent recipient.
    ensure_agent_exists(&mut connection, &keys, from, "sender").await?;
    ensure_agent_exists(&mut connection, &keys, to, "recipient").await?;

    let event = MessageEvent {
        from: from.to_string(),
        to: to.to_string(),
        body: body.to_string(),
        // Record send time as a Unix-timestamp string for stable ordering display.
        created_at: team::unix_timestamp_string(),
    };

    // Append to the stream. The `*` id lets Redis assign a monotonic id;
    // the message fields are stored as a flat key/value list.
    let id: String = redis::cmd("XADD")
        .arg(keys.stream())
        .arg("*")
        .arg("from")
        .arg(&event.from)
        .arg("to")
        .arg(&event.to)
        .arg("body")
        .arg(&event.body)
        .arg("created_at")
        .arg(&event.created_at)
        .query_async(&mut connection)
        .await?;

    // Best-effort notification: publish the new entry id so watch/REPL
    // subscribers can react without scanning the stream. The returned count
    // of subscribers is intentionally ignored — delivery is not guaranteed.
    let _: usize = connection.publish(keys.pubsub(), &id).await?;

    Ok(MessageRecord { id, event })
}

/// Read messages addressed to `agent` that arrived after its read cursor.
///
/// This is the testable core behind `inbox`. It reads the agent's stored
/// cursor (defaulting to the start of the stream), scans the stream for
/// entries strictly newer than that cursor, keeps only those addressed to
/// `agent`, and — when `mode` is `InboxReadMode::MarkRead` — advances the
/// cursor to the last entry it observed.
///
/// # Parameters
///
/// - `url`: Redis connection URL.
/// - `team`: team name used to derive all Redis keys.
/// - `agent`: recipient whose inbox is being read (must be a team agent).
/// - `mode`: whether to advance the cursor or merely peek.
///
/// # Returns
///
/// The list of `MessageRecord`s addressed to `agent`, in stream order.
///
/// # Errors
///
/// Returns `MagiError::NotFound` if `agent` is not a team agent, or any
/// Redis error from connecting, reading the cursor, scanning the stream,
/// or writing the updated cursor.
pub async fn read_inbox_with_url(
    url: &str,
    team: &str,
    agent: &str,
    mode: InboxReadMode,
) -> Result<Vec<MessageRecord>> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    ensure_agent_exists(&mut connection, &keys, agent, "recipient").await?;

    // Load the per-agent cursor; absence means "never read", so start from
    // the synthetic id `0-0` (before any real stream entry).
    let cursor: Option<String> = connection.get(keys.cursor(agent)).await?;
    let cursor = cursor.unwrap_or_else(|| "0-0".to_string());
    // The leading `(` makes the XRANGE start bound exclusive, so the cursor
    // entry itself is skipped and only strictly newer messages are returned.
    let start = format!("({cursor}");
    let entries = stream_range(&mut connection, &keys.stream(), &start, "+").await?;
    // Remember the newest id seen across ALL scanned entries (not just the
    // ones addressed to this agent) so the cursor can skip past unrelated
    // traffic too and we never re-scan it.
    let last_seen_id = entries.last().map(|message| message.id.clone());
    let messages = entries
        .into_iter()
        .filter(|message| message.event.to == agent)
        .collect::<Vec<_>>();

    // Only persist progress when consuming; Peek leaves the cursor intact.
    if mode == InboxReadMode::MarkRead {
        if let Some(last_seen_id) = last_seen_id {
            let _: () = connection.set(keys.cursor(agent), last_seen_id).await?;
        }
    }

    Ok(messages)
}

/// Read the entire team stream, optionally filtered to one agent.
///
/// This is the testable core behind `history`. It scans the whole stream
/// (`-` to `+`, the full id range) without consulting or mutating any
/// cursor, so it is purely non-destructive. When `agent` is `Some`, only
/// messages where that agent is the sender or the recipient are returned.
///
/// # Errors
///
/// Returns any Redis error from connecting or scanning the stream.
pub async fn history_with_url(
    url: &str,
    team: &str,
    agent: Option<&str>,
) -> Result<Vec<MessageRecord>> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    // `-`..`+` is the full id range: every entry ever written to the stream.
    let mut messages = stream_range(&mut connection, &keys.stream(), "-", "+").await?;

    // Optional client-side filter: keep only messages involving the agent.
    if let Some(agent) = agent {
        messages.retain(|message| message.event.from == agent || message.event.to == agent);
    }

    Ok(messages)
}

/// Render a message as a single human-readable line for CLI output.
///
/// The format is `[<created_at>] <from> -> <to>: <body>`, shared by the
/// `inbox` and `history` subcommands so their output is consistent.
pub fn format_message_line(message: &MessageRecord) -> String {
    format!(
        "[{}] {} -> {}: {}",
        message.event.created_at, message.event.from, message.event.to, message.event.body
    )
}

/// Verify that `agent` is a registered member of the team's agent set.
///
/// `role` is a label (for example `"sender"` or `"recipient"`) used only to
/// produce a clearer error message; it does not affect the lookup.
///
/// # Errors
///
/// Returns `MagiError::NotFound` when the agent is absent from the team's
/// agent set, or any Redis error from the membership check.
async fn ensure_agent_exists(
    connection: &mut redis::aio::MultiplexedConnection,
    keys: &RedisKeys,
    agent: &str,
    role: &str,
) -> Result<()> {
    // Agents are tracked in a Redis set; `SISMEMBER` is an O(1) membership test.
    let exists: bool = connection.sismember(keys.team_agents(), agent).await?;
    if !exists {
        return Err(MagiError::NotFound(format!("{role} `{agent}`")));
    }
    Ok(())
}

/// Scan a Redis Stream over `[start, end]` and decode every entry.
///
/// Wraps `XRANGE`, whose reply shape is a list of `(entry_id, [(field,
/// value), ...])` pairs. The `start`/`end` bounds use Redis id syntax:
/// `-`/`+` for the full range, a bare id for an inclusive bound, or a
/// `(`-prefixed id for an exclusive bound (used by the inbox cursor).
///
/// # Errors
///
/// Returns any Redis error from `XRANGE`, or a decode error if an entry is
/// missing a required field (see `message_from_stream_fields`).
async fn stream_range(
    connection: &mut redis::aio::MultiplexedConnection,
    stream: &str,
    start: &str,
    end: &str,
) -> Result<Vec<MessageRecord>> {
    // XRANGE returns entries as (id, flat field/value pairs); decode each below.
    let entries: Vec<(String, Vec<(String, String)>)> = redis::cmd("XRANGE")
        .arg(stream)
        .arg(start)
        .arg(end)
        .query_async(connection)
        .await?;

    // Decode every entry; the first decode failure short-circuits via collect.
    entries
        .into_iter()
        .map(|(id, fields)| message_from_stream_fields(id, fields))
        .collect()
}

/// Decode one stream entry's flat field list into a `MessageRecord`.
///
/// Gathers the `from`, `to`, `body`, and `created_at` fields written by
/// `send_message_with_url`; unrecognized fields are ignored for forward
/// compatibility. The `id` is the entry's stream id, passed through verbatim.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` if any of the four required fields
/// is missing from the entry.
fn message_from_stream_fields(id: String, fields: Vec<(String, String)>) -> Result<MessageRecord> {
    let mut from = None;
    let mut to = None;
    let mut body = None;
    let mut created_at = None;

    // Walk the flat (key, value) list; collect the known fields and skip any
    // others so future schema additions do not break decoding.
    for (key, value) in fields {
        match key.as_str() {
            "from" => from = Some(value),
            "to" => to = Some(value),
            "body" => body = Some(value),
            "created_at" => created_at = Some(value),
            _ => {}
        }
    }

    Ok(MessageRecord {
        id,
        event: MessageEvent {
            from: from.ok_or_else(|| {
                MagiError::InvalidConfig("stream message is missing from field".to_string())
            })?,
            to: to.ok_or_else(|| {
                MagiError::InvalidConfig("stream message is missing to field".to_string())
            })?,
            body: body.ok_or_else(|| {
                MagiError::InvalidConfig("stream message is missing body field".to_string())
            })?,
            created_at: created_at.ok_or_else(|| {
                MagiError::InvalidConfig("stream message is missing created_at field".to_string())
            })?,
        },
    })
}

/// Extract the configured Redis URL, or fail if `redis.url` is unset.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` when `redis.url` is not configured.
fn configured_redis_url(config: &AppConfig) -> Result<String> {
    config
        .redis
        .url
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("redis.url is not configured".to_string()))
}

/// Extract the active team name, or fail if `identity.active_team` is unset.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` when no active team is configured.
fn active_team(config: &AppConfig) -> Result<String> {
    config
        .identity
        .active_team
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("identity.active_team is required".to_string()))
}

/// Extract the active agent name, or fail if `identity.active_agent` is unset.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` when no active agent is configured.
fn active_agent(config: &AppConfig) -> Result<String> {
    config
        .identity
        .active_agent
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("identity.active_agent is required".to_string()))
}
