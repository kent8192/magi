//! Invite-based onboarding flow for the `magi` CLI.
//!
//! This module implements the lifecycle of team invitations, which is how a new
//! agent gains access to an existing team. The flow has four user-facing stages,
//! each backed by a CLI command and a `*_with_url` worker function that talks to
//! Redis directly (so they can be reused and unit-tested without re-loading config):
//!
//! - `create`: an owner mints a single-use-style invite for a team and prints the
//!   secret token. The token is shown only once; only its SHA-256 hash is stored.
//! - `list`: enumerate the invites that belong to a team for inspection.
//! - `revoke`: invalidate an outstanding invite so its token can no longer be used.
//! - `join`: a new agent redeems an invite token, atomically consuming it and
//!   registering itself as a member of the invite's team.
//!
//! # Security model
//!
//! The raw invite token is never persisted. At creation time only its hash (see
//! `token_hash`) is written to Redis, and a short-lived lookup key maps that hash
//! back to the owning invite record. Redemption (`join_with_url`) re-hashes the
//! presented token, looks up the invite, and runs a server-side Lua script
//! (`lua/consume_invite.lua`) so that validity checks (revoked / expired / max-uses)
//! and the usage-count increment happen as a single atomic operation, preventing
//! races between concurrent redeemers.
//!
//! # Redis key layout
//!
//! Keys are derived from `RedisKeys`:
//! - `<team>:invite:<invite_id>` — a hash holding the invite's metadata.
//! - `<team>:invites` — a set of all invite ids belonging to the team.
//! - the token-lookup key from `RedisKeys::invite_token` — a TTL-bounded string
//!   mapping a token hash to its invite record key, used for redemption.

use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use redis::AsyncCommands;
use sha2::{Digest, Sha256};

use crate::config::AppConfig;
use crate::error::{MagiError, Result};
use crate::model::RedisKeys;
use crate::redis_client;
use crate::team;

/// Result of successfully minting a new invite via `create_invite_with_url`.
///
/// This is the only place the raw `token` is available, since Redis stores the
/// hash rather than the secret itself. Callers should surface the token to the
/// user immediately and then drop it; it cannot be recovered later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedInvite {
    /// Public identifier of the invite, of the form `inv_<random>`.
    pub invite_id: String,
    /// Secret redemption token. Available only at creation time and never persisted.
    pub token: String,
    /// SHA-256 (URL-safe base64) hash of `token`, as stored in Redis.
    pub token_hash: String,
    /// Team the invite grants membership to.
    pub team: String,
    /// Unix timestamp (seconds) after which the invite expires.
    pub expires_at: u64,
}

/// A read-only view of a stored invite, as returned by `list_invites_with_url`.
///
/// Timestamp fields are kept as raw `String`s because they are read back verbatim
/// from the Redis hash (where they were written as seconds-since-epoch). Fields
/// that may be absent in the hash default to empty strings or zero during decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteSummary {
    /// Public identifier of the invite (`inv_<random>`).
    pub invite_id: String,
    /// Team the invite belongs to.
    pub team: String,
    /// Identity that created the invite (the owner / active agent).
    pub created_by: String,
    /// Creation time as a Unix-seconds string.
    pub created_at: String,
    /// Expiry time as a Unix-seconds string.
    pub expires_at: String,
    /// Number of times the invite has been redeemed so far.
    pub used_count: u64,
    /// Maximum allowed redemptions; `0` means unbounded in the current scheme.
    pub max_uses: u64,
    /// Revocation time as a Unix-seconds string, or `None` if still active.
    pub revoked_at: Option<String>,
}

/// Tuple shape returned by the pipelined `HGET`s in `list_invites_with_url`.
///
/// Each element is `Option` because the corresponding hash field may be missing
/// (e.g. an invite that was never revoked has no `revoked_at`). The field order
/// mirrors the order of the `hget` calls: team, created_by, created_at,
/// expires_at, used_count, max_uses, revoked_at.
type RawInviteSummary = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<u64>,
    Option<u64>,
    Option<String>,
);

/// CLI entry point for `magi invite create`: mints an invite and prints its token.
///
/// Loads the application config, resolves the configured Redis URL, derives the
/// creator identity from the active agent (falling back to `"owner"`), parses the
/// human-readable `ttl` (e.g. `"24h"`), and delegates to `create_invite_with_url`.
/// Only the secret token is printed to stdout — it is shown once and not stored.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded, `redis.url` is unset, the
/// `ttl` string is malformed (see `parse_ttl`), or the Redis write fails.
pub async fn create(team: String, ttl: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    // Attribute the invite to the active agent when one is configured;
    // otherwise fall back to a generic "owner" label.
    let created_by = config
        .identity
        .active_agent
        .as_deref()
        .unwrap_or("owner")
        .to_string();
    let ttl = parse_ttl(&ttl)?;
    let invite = create_invite_with_url(&url, &team, &created_by, ttl).await?;

    // Print only the secret token: this is the single moment it is visible.
    println!("Invite: {}", invite.token);
    Ok(())
}

/// CLI entry point for `magi invite list`: prints one line per invite of a team.
///
/// Loads config, resolves the Redis URL, and renders each `InviteSummary` from
/// `list_invites_with_url` as a space-separated key=value line. A missing
/// `revoked_at` is rendered as an empty `revoked=` field.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded, `redis.url` is unset, or the
/// Redis reads fail.
pub async fn list(team: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let invites = list_invites_with_url(&url, &team).await?;

    // Render each invite as a single human-readable line for the terminal.
    for invite in invites {
        println!(
            "{} team={} created_by={} used={} max_uses={} revoked={}",
            invite.invite_id,
            invite.team,
            invite.created_by,
            invite.used_count,
            invite.max_uses,
            invite.revoked_at.as_deref().unwrap_or("")
        );
    }

    Ok(())
}

/// CLI entry point for `magi invite revoke`: invalidates an outstanding invite.
///
/// The team is taken from the caller's active identity rather than an argument,
/// so an agent can only revoke invites of the team it is currently scoped to.
/// Delegates the actual mutation to `revoke_invite_with_url`.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded, `redis.url` is unset,
/// `identity.active_team` is not set, the invite does not belong to the active
/// team, or the Redis writes fail.
pub async fn revoke(invite_id: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    // Revocation is scoped to the active team; an explicit team is intentionally
    // not accepted on the command line to avoid cross-team revocation.
    let team = config
        .identity
        .active_team
        .ok_or_else(|| MagiError::InvalidConfig("identity.active_team is required".to_string()))?;

    revoke_invite_with_url(&url, &team, &invite_id).await?;
    println!("Revoked invite: {invite_id}");
    Ok(())
}

/// CLI entry point for `magi invite join`: redeems an invite token to join a team.
///
/// Derives the joining agent's name from the active identity (defaulting to
/// `"agent"`) and records the current working directory as the project context.
/// The agent type is currently hard-coded to `"codex"`. Delegates the atomic
/// consume-and-register flow to `join_with_url`.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded, `redis.url` is unset, the
/// current directory cannot be read, the token is invalid / revoked / expired /
/// exhausted, or agent registration fails.
pub async fn join(invite: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    // Name the joining agent from the active identity, defaulting to "agent".
    let agent = config
        .identity
        .active_agent
        .as_deref()
        .unwrap_or("agent")
        .to_string();
    // Record the directory the command was run in as the agent's project path.
    let project = std::env::current_dir()?.display().to_string();

    let team = join_with_url(&url, &invite, &agent, "codex", &project).await?;
    println!("Joined team {team} as {agent}");
    Ok(())
}

/// Mints a new invite for `team` against the Redis instance at `url`.
///
/// Generates a random public `invite_id` and a separate secret `token`, then
/// stores the invite's metadata hash, adds the id to the team's invite set, and
/// writes a TTL-bounded token-lookup key mapping the token's hash to the invite
/// record. The raw token is returned to the caller but never persisted.
///
/// The metadata hash itself is not given an explicit TTL; the lookup key (which
/// is what redemption resolves through) is what expires after `ttl`.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` if `ttl` is zero, or a Redis error if
/// connecting or the pipelined write fails.
pub async fn create_invite_with_url(
    url: &str,
    team: &str,
    created_by: &str,
    ttl: Duration,
) -> Result<CreatedInvite> {
    // Reject zero TTLs up front: a never-valid invite is almost certainly a bug.
    if ttl.is_zero() {
        return Err(MagiError::InvalidConfig(
            "invite ttl must be greater than zero".to_string(),
        ));
    }

    let keys = RedisKeys::new(team);
    // Public, shareable identifier (16 random bytes) used as the Redis hash key.
    let invite_id = format!("inv_{}", random_urlsafe(16));
    // Secret redemption token (32 random bytes); only its hash is stored.
    let token = random_urlsafe(32);
    let token_hash = token_hash(&token);
    // Lookup key keyed by the token hash, enabling O(1) redemption by token.
    let lookup_key = RedisKeys::invite_token(&token_hash);
    let now = now_secs();
    let expires_at = now + ttl.as_secs();
    let mut connection = redis_client::connect(url).await?;

    // Write all invite state in a single MULTI/EXEC transaction (`.atomic()`) so
    // the metadata hash, the team's invite-set membership, and the token-lookup
    // key either all appear together or not at all. The lookup key carries the
    // TTL via SET ... EX, so an expired invite naturally becomes unredeemable.
    let _: () = redis::pipe()
        .atomic()
        .hset(keys.invite(&invite_id), "invite_id", &invite_id)
        .hset(keys.invite(&invite_id), "team", team)
        .hset(keys.invite(&invite_id), "created_by", created_by)
        .hset(keys.invite(&invite_id), "created_at", now)
        .hset(keys.invite(&invite_id), "expires_at", expires_at)
        .hset(keys.invite(&invite_id), "token_hash", &token_hash)
        .hset(keys.invite(&invite_id), "used_count", 0)
        .hset(keys.invite(&invite_id), "max_uses", 0)
        .sadd(format!("{}:invites", keys.team()), &invite_id)
        .set_ex(&lookup_key, keys.invite(&invite_id), ttl.as_secs())
        .query_async(&mut connection)
        .await?;

    Ok(CreatedInvite {
        invite_id,
        token,
        token_hash,
        team: team.to_string(),
        expires_at,
    })
}

/// Lists all invites belonging to `team`, sorted by `invite_id`.
///
/// Reads the team's `<team>:invites` set, then fetches each invite's metadata
/// hash. Ids whose hash no longer exists (e.g. expired and cleaned up) are
/// skipped, as are records whose stored `team` does not match — a defensive
/// guard against stale or cross-team set membership.
///
/// # Errors
///
/// Returns a Redis error if connecting, reading the set, or any per-invite
/// read fails.
pub async fn list_invites_with_url(url: &str, team: &str) -> Result<Vec<InviteSummary>> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    // Fetch the set of invite ids for the team, then sort for stable output.
    let mut invite_ids: Vec<String> = connection
        .smembers(format!("{}:invites", keys.team()))
        .await?;
    invite_ids.sort();

    let mut invites = Vec::with_capacity(invite_ids.len());
    for invite_id in invite_ids {
        let invite_key = keys.invite(&invite_id);
        // The set may still reference invites whose hash has expired; skip those
        // rather than emitting empty rows.
        let exists: bool = connection.exists(&invite_key).await?;
        if !exists {
            continue;
        }

        // Read all wanted fields in one round trip via a pipeline. The tuple
        // element order must match the sequence of hget calls below.
        let (
            stored_team,
            created_by,
            created_at,
            expires_at,
            used_count,
            max_uses,
            revoked_at,
        ): RawInviteSummary = redis::pipe()
            .hget(&invite_key, "team")
            .hget(&invite_key, "created_by")
            .hget(&invite_key, "created_at")
            .hget(&invite_key, "expires_at")
            .hget(&invite_key, "used_count")
            .hget(&invite_key, "max_uses")
            .hget(&invite_key, "revoked_at")
            .query_async(&mut connection)
            .await?;

        // Defensive consistency check: ignore records whose stored team does not
        // match the requested team (guards against stale set membership).
        if stored_team.as_deref() != Some(team) {
            continue;
        }

        // Decode the raw Optionals into a summary, defaulting missing fields.
        invites.push(InviteSummary {
            invite_id,
            team: stored_team.unwrap_or_default(),
            created_by: created_by.unwrap_or_default(),
            created_at: created_at.unwrap_or_default(),
            expires_at: expires_at.unwrap_or_default(),
            used_count: used_count.unwrap_or(0),
            max_uses: max_uses.unwrap_or(0),
            revoked_at,
        });
    }

    Ok(invites)
}

/// Redeems an invite `token` and registers the agent into the invite's team.
///
/// Hashes the presented token, resolves it through the token-lookup key, and
/// atomically validates-and-consumes the invite via a server-side Lua script
/// (see `consume_invite_atomically`). On success it registers the agent with
/// the team. Returns the name of the team that was joined.
///
/// # Compensation on failure
///
/// The invite is consumed (its `used_count` incremented) *before* agent
/// registration. If registration then fails, this function rolls the count back
/// by decrementing `used_count`, so a failed join does not permanently burn a
/// use of the invite. The rollback result is intentionally ignored — there is no
/// useful recovery if it also fails, and the original error is what the caller
/// needs.
///
/// # Errors
///
/// Returns an error if connecting fails, the token is invalid / revoked /
/// expired / exhausted, or agent registration fails (after best-effort rollback).
pub async fn join_with_url(
    url: &str,
    token: &str,
    agent: &str,
    agent_type: &str,
    project: &str,
) -> Result<String> {
    // Re-derive the hash from the presented token and resolve the lookup key.
    let hash = token_hash(token);
    let lookup_key = RedisKeys::invite_token(&hash);
    let mut connection = redis_client::connect(url).await?;
    // Atomically validate and consume one use of the invite (Lua-scripted).
    let consumed = consume_invite_atomically(&mut connection, &lookup_key, &hash).await?;
    let keys = RedisKeys::new(&consumed.team);

    if let Err(error) =
        team::register_agent_with_connection(&mut connection, &keys, agent, agent_type, project)
            .await
    {
        // Registration failed after the invite was already consumed: compensate
        // by decrementing the usage count so this attempt does not waste a use.
        // The rollback's own outcome is deliberately discarded; we propagate the
        // original registration error instead.
        let _: std::result::Result<i64, redis::RedisError> = connection
            .hincr(&consumed.invite_key, "used_count", -1)
            .await;
        return Err(error);
    }

    Ok(consumed.team)
}

/// Revokes the invite `invite_id` of `team` against the Redis at `url`.
///
/// Verifies the invite exists and actually belongs to `team`, then atomically
/// stamps `revoked_at` on the metadata hash and deletes the token-lookup key so
/// the token can no longer be resolved during a join. The metadata hash is kept
/// (with `revoked_at` set) so the invite still appears in `list_invites_with_url`.
///
/// # Errors
///
/// Returns `MagiError::NotFound` if the invite is missing, belongs to a
/// different team, or has no stored `token_hash`; or a Redis error if the
/// read/write fails.
pub async fn revoke_invite_with_url(url: &str, team: &str, invite_id: &str) -> Result<()> {
    let keys = RedisKeys::new(team);
    let invite_key = keys.invite(invite_id);
    let mut connection = redis_client::connect(url).await?;
    // Read the stored team and token hash in one round trip to validate ownership
    // and locate the lookup key that must be deleted.
    let (stored_team, token_hash): (Option<String>, Option<String>) = redis::pipe()
        .hget(&invite_key, "team")
        .hget(&invite_key, "token_hash")
        .query_async(&mut connection)
        .await?;
    // Refuse to revoke an invite that does not belong to the requested team
    // (also covers the "invite does not exist" case, where stored_team is None).
    if stored_team.as_deref() != Some(team) {
        return Err(MagiError::NotFound(format!("invite `{invite_id}`")));
    }
    let token_hash =
        token_hash.ok_or_else(|| MagiError::NotFound(format!("invite `{invite_id}`")))?;

    let lookup_key = RedisKeys::invite_token(&token_hash);
    // Atomically mark the invite revoked and remove its token-lookup key, so the
    // record survives for listing while the token becomes unresolvable.
    let _: () = redis::pipe()
        .atomic()
        .hset(&invite_key, "revoked_at", now_secs())
        .del(lookup_key)
        .query_async(&mut connection)
        .await?;

    Ok(())
}

/// Parses a human-readable TTL string such as `"24h"`, `"30m"`, or `"45s"`.
///
/// The format is `<positive-integer><unit>`, where `unit` is one of `h` (hours),
/// `m` (minutes), or `s` (seconds). The numeric part is everything before the
/// final character.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` if the string is empty, lacks a numeric
/// part, has a non-numeric amount, uses an unrecognized unit, or evaluates to
/// zero (a zero TTL is rejected as invalid).
pub fn parse_ttl(value: &str) -> Result<Duration> {
    // Split off the trailing unit character; saturating_sub guards the empty input.
    let Some((number, suffix)) = value.split_at_checked(value.len().saturating_sub(1)) else {
        return Err(MagiError::InvalidConfig(
            "invite ttl is invalid".to_string(),
        ));
    };
    // A missing numeric part (e.g. just a bare unit like "h") is invalid.
    if number.is_empty() {
        return Err(MagiError::InvalidConfig(
            "invite ttl is invalid".to_string(),
        ));
    }

    let amount: u64 = number
        .parse()
        .map_err(|_| MagiError::InvalidConfig("invite ttl is invalid".to_string()))?;
    if amount == 0 {
        return Err(MagiError::InvalidConfig(
            "invite ttl must be greater than zero".to_string(),
        ));
    }

    // Convert the amount to seconds based on the unit suffix.
    let seconds = match suffix {
        "h" => amount * 60 * 60,
        "m" => amount * 60,
        "s" => amount,
        _ => {
            return Err(MagiError::InvalidConfig(
                "invite ttl is invalid".to_string(),
            ))
        }
    };

    Ok(Duration::from_secs(seconds))
}

/// Computes the storage hash of an invite `token`.
///
/// Hashes the token with SHA-256 and encodes the digest as URL-safe base64
/// without padding. This is the value persisted in Redis (the raw token is
/// never stored), and is recomputed at redemption time to look the invite up.
pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Outcome of a successful atomic invite consumption.
///
/// Carries just enough to continue the join: the team being joined and the
/// Redis key of the consumed invite record (needed to roll back `used_count`
/// if subsequent agent registration fails).
struct ConsumedInvite {
    /// Team the consumed invite grants membership to.
    team: String,
    /// Redis key of the invite's metadata hash.
    invite_key: String,
}

/// Atomically validates and consumes one use of an invite via a Lua script.
///
/// Runs `lua/consume_invite.lua` (embedded at compile time via `include_str!`)
/// server-side so that the validity checks (token match, not revoked, not
/// expired, under max uses) and the `used_count` increment execute as one
/// indivisible operation. This prevents two concurrent redeemers from both
/// passing the checks and over-consuming a single-use invite.
///
/// Script contract:
/// - `KEYS[1]` is the token-lookup key.
/// - `ARGV[1]` is the expected token hash; `ARGV[2]` is the current time.
/// - The reply is a string array whose first element is a status word. On `"ok"`
///   the array has length 3: `["ok", <team>, <invite_key>]`.
///
/// # Errors
///
/// Maps each non-`ok` status word to a `MagiError`: `invalid` → token not
/// found, `revoked` / `expired` / `max_uses` → invalid-config errors. Any
/// unexpected reply shape yields a generic invalid-config error. Redis transport
/// errors are propagated.
async fn consume_invite_atomically(
    connection: &mut redis::aio::MultiplexedConnection,
    lookup_key: &str,
    expected_hash: &str,
) -> Result<ConsumedInvite> {
    // Pass the lookup key, the expected token hash, and the current time so all
    // freshness and validity decisions are made atomically inside Redis.
    let response: Vec<String> = redis::Script::new(include_str!("lua/consume_invite.lua"))
        .key(lookup_key)
        .arg(expected_hash)
        .arg(now_secs())
        .invoke_async(connection)
        .await?;

    // Translate the script's status word into a typed result/error. The "ok"
    // branch additionally requires the full 3-element payload to be present.
    match response.first().map(String::as_str) {
        Some("ok") if response.len() == 3 => Ok(ConsumedInvite {
            team: response[1].clone(),
            invite_key: response[2].clone(),
        }),
        Some("invalid") => Err(MagiError::NotFound("invite token".to_string())),
        Some("revoked") => Err(MagiError::InvalidConfig("invite is revoked".to_string())),
        Some("expired") => Err(MagiError::InvalidConfig("invite is expired".to_string())),
        Some("max_uses") => Err(MagiError::InvalidConfig(
            "invite has reached maximum uses".to_string(),
        )),
        _ => Err(MagiError::InvalidConfig(
            "invalid invite consumption response".to_string(),
        )),
    }
}

/// Generates `bytes` random bytes and returns them URL-safe base64 encoded.
///
/// Used to mint both invite ids and secret tokens. The returned string is longer
/// than `bytes` because of base64 expansion; `bytes` controls entropy, not the
/// output length.
fn random_urlsafe(bytes: usize) -> String {
    // Fill a buffer with random bytes, then encode without padding so the result
    // is safe to embed in URLs and Redis keys.
    let mut buffer = vec![0_u8; bytes];
    for byte in &mut buffer {
        *byte = rand::random();
    }
    URL_SAFE_NO_PAD.encode(buffer)
}

/// Returns the current Unix time in whole seconds.
///
/// Used for invite `created_at` / `expires_at` / `revoked_at` stamps and as the
/// "now" argument to the consume script. If the system clock is somehow before
/// the Unix epoch, the duration defaults to zero rather than panicking.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Extracts the configured Redis connection URL from the application config.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` if `redis.url` is not set, since every
/// invite operation requires a Redis backend.
fn configured_redis_url(config: &AppConfig) -> Result<String> {
    config
        .redis
        .url
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("redis.url is not configured".to_string()))
}
