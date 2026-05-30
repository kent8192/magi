//! Core data models for the magi messaging system.
//!
//! This module defines the fundamental types used throughout the codebase:
//!
//! - `RedisKeys` — a factory for all Redis key strings used by magi, ensuring
//!   consistent naming across streams, pub/sub channels, agent registrations,
//!   invite tokens, and team metadata.
//! - `AgentIdentity` — the serializable identity of a connected agent (name +
//!   team), stored in Redis and exchanged during the onboarding handshake.
//! - `MessageEvent` — a single message payload written to a Redis Stream entry
//!   and delivered to recipient agents.
//!
//! All user-supplied strings that appear in Redis key segments are sanitised by
//! `encode_key_segment` before use, so colons and other special characters
//! cannot corrupt the key hierarchy.

use serde::{Deserialize, Serialize};

/// The top-level namespace prefix shared by every Redis key that magi creates.
///
/// All keys follow the pattern `magi:<category>:<team_id>[:<sub_key>]`.
/// Using a common prefix makes it easy to inspect or flush magi state without
/// touching unrelated keys in a shared Redis instance.
pub const REDIS_KEY_PREFIX: &str = "magi";

/// A scoped key-builder for all Redis keys that belong to a given team.
///
/// Construct a `RedisKeys` instance once per operation with the target
/// `team_id`, then call the appropriate method to obtain the fully-qualified
/// Redis key string for that operation.  The `team_id` is URL-percent-encoded
/// on construction so that special characters (colons, spaces, etc.) cannot
/// break the key hierarchy.
///
/// # Key layout
///
/// ```text
/// magi:teams                         — sorted-set of all team IDs
/// magi:team:<team_id>                — hash of team metadata
/// magi:team:<team_id>:agents         — set of agent IDs in the team
/// magi:agent:<team_id>:<agent_id>    — hash of agent metadata
/// magi:agent:<team_id>:<agent_id>:registrations — registration counter / set
/// magi:stream:<team_id>              — Redis Stream carrying team messages
/// magi:cursor:<team_id>:<agent_id>   — last-read stream entry ID for an agent
/// magi:pubsub:<team_id>              — Pub/Sub channel for real-time delivery
/// magi:invite:<invite_id>            — hash of a single invite record
/// magi:invite_token:<token_hash>     — maps a token hash to an invite ID
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisKeys {
    /// The percent-encoded team identifier used in every key produced by this
    /// instance.  Encoding is applied once at construction time.
    team_id: String,
}

impl RedisKeys {
    /// Creates a new `RedisKeys` scoped to `team_id`.
    ///
    /// The provided `team_id` is immediately passed through `encode_key_segment`
    /// so that all keys returned by subsequent method calls are safe to use
    /// directly in Redis commands without further escaping.
    pub fn new(team_id: impl Into<String>) -> Self {
        Self {
            team_id: encode_key_segment(&team_id.into()),
        }
    }

    /// Returns the global sorted-set key that lists every registered team.
    ///
    /// This key is shared across all teams; members are team ID strings.
    pub fn teams(&self) -> String {
        format!("{REDIS_KEY_PREFIX}:teams")
    }

    /// Returns the hash key that holds metadata for this team.
    ///
    /// Fields stored in this hash include the team name, creation timestamp,
    /// and any configuration set during team provisioning.
    pub fn team(&self) -> String {
        format!("{REDIS_KEY_PREFIX}:team:{}", self.team_id)
    }

    /// Returns the set key that holds the agent IDs belonging to this team.
    ///
    /// Membership in this set determines which agents may read from and write
    /// to the team's stream.
    pub fn team_agents(&self) -> String {
        format!("{}:agents", self.team())
    }

    /// Returns the hash key that stores metadata for a specific agent within
    /// this team.
    ///
    /// `agent_id` is percent-encoded before being embedded in the key so that
    /// agent names with special characters are handled safely.
    pub fn agent(&self, agent_id: &str) -> String {
        format!(
            "{REDIS_KEY_PREFIX}:agent:{}:{}",
            self.team_id,
            encode_key_segment(agent_id)
        )
    }

    /// Returns the key used to track an agent's session registrations within
    /// this team.
    ///
    /// This is used to detect duplicate connections and to clean up stale
    /// agent state when a session terminates.
    pub fn registrations(&self, agent_id: &str) -> String {
        format!("{}:registrations", self.agent(agent_id))
    }

    /// Returns the Redis Stream key through which all team messages flow.
    ///
    /// Every `MessageEvent` sent within the team is appended to this stream
    /// via `XADD`.  Agents consume from it using `XREAD` (blocking reads) with
    /// their per-agent cursor stored at the key returned by `RedisKeys::cursor`.
    pub fn stream(&self) -> String {
        format!("{REDIS_KEY_PREFIX}:stream:{}", self.team_id)
    }

    /// Returns the key that persists the last stream entry ID consumed by
    /// `agent_id`.
    ///
    /// The cursor is updated after each successful `XREAD` so that the agent
    /// resumes from the correct position after a reconnect.  Storing it in
    /// Redis (rather than in process memory) allows the position to survive
    /// process restarts.
    pub fn cursor(&self, agent_id: &str) -> String {
        format!(
            "{REDIS_KEY_PREFIX}:cursor:{}:{}",
            self.team_id,
            encode_key_segment(agent_id)
        )
    }

    /// Returns the Pub/Sub channel name used for real-time message delivery
    /// within this team.
    ///
    /// Agents subscribe to this channel to receive low-latency notifications
    /// when a new message is available on the stream.  The stream itself
    /// remains the source of truth; the pub/sub channel is a delivery hint
    /// that wakes up blocked readers without polling.
    pub fn pubsub(&self) -> String {
        format!("{REDIS_KEY_PREFIX}:pubsub:{}", self.team_id)
    }

    /// Returns the hash key for a specific invite record.
    ///
    /// Invite records are created during the invite-based onboarding flow and
    /// contain the target team, expiry, and usage limits.  `invite_id` is
    /// percent-encoded before embedding.
    pub fn invite(&self, invite_id: &str) -> String {
        format!(
            "{REDIS_KEY_PREFIX}:invite:{}",
            encode_key_segment(invite_id)
        )
    }

    /// Returns the key that maps a hashed invite token to its `invite_id`.
    ///
    /// This is a team-independent (static) method because invite tokens are
    /// looked up before the team identity is known.  `token_hash` should be
    /// the hex-encoded SHA-256 (or equivalent) of the raw token so that the
    /// plaintext secret is never stored in Redis.
    pub fn invite_token(token_hash: &str) -> String {
        format!(
            "{REDIS_KEY_PREFIX}:invite_token:{}",
            encode_key_segment(token_hash)
        )
    }
}

/// Encodes an arbitrary string so it is safe to embed as a Redis key segment.
///
/// Only alphanumeric characters and the `-` / `_` symbols are passed through
/// unchanged.  Every other byte is replaced with a percent-encoded sequence
/// (`%XX` where `XX` is the uppercase hex value of the byte), mirroring the
/// percent-encoding scheme used in URLs.
///
/// This prevents user-supplied strings such as team names or agent IDs from
/// introducing `:` characters that would corrupt the `magi:<category>:<id>`
/// key hierarchy.
///
/// # Example
///
/// ```text
/// "my team"  →  "my%20team"
/// "a:b"      →  "a%3Ab"
/// "ok-id_1"  →  "ok-id_1"
/// ```
fn encode_key_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());

    for byte in segment.bytes() {
        // Allow characters that are safe to appear literally in a Redis key
        // segment without ambiguity.  Everything else is percent-encoded.
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => encoded.push(byte as char),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }

    encoded
}

/// The serialisable identity of an agent that has joined a magi team.
///
/// An `AgentIdentity` is created during the invite-acceptance / onboarding
/// flow and stored as a Redis hash entry under the key produced by
/// `RedisKeys::agent`.  It is also embedded in outgoing `MessageEvent`
/// records so recipients know who sent each message.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentIdentity {
    /// Human-readable name chosen by the agent operator.  This is the
    /// display name used in message headers and team-member listings.
    pub name: String,
    /// The team that this agent belongs to.  Must match a team ID registered
    /// in the global teams set (see `RedisKeys::teams`).
    pub team: String,
}

/// A single message delivered through a team's Redis Stream.
///
/// Each `MessageEvent` is serialised to JSON and stored as the value of an
/// `XADD` entry in the stream identified by `RedisKeys::stream`.  Consumers
/// deserialise the entry back into a `MessageEvent` after reading it with
/// `XREAD`.
///
/// The `to` field may be an explicit agent name for direct messages or a
/// broadcast sentinel (e.g. `"*"`) recognised by the watch-mode reader.
#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct MessageEvent {
    /// The agent name of the sender, as recorded in `AgentIdentity::name`.
    pub from: String,
    /// The intended recipient agent name, or a broadcast indicator.
    pub to: String,
    /// The text content of the message.
    pub body: String,
    /// ISO-8601 UTC timestamp at which the message was created by the sender.
    pub created_at: String,
}
