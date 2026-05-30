//! Team membership operations for the `magi` CLI.
//!
//! A *team* groups the CLI AI agents that can message one another. This module
//! owns the lifecycle of teams and their members on top of Redis: creating a
//! team (and registering its initial owner), listing all known teams, listing
//! the members of a team, and registering or refreshing an individual agent's
//! membership.
//!
//! All persistent state lives in Redis under keys derived from `RedisKeys`.
//! The functions here come in two flavours:
//!
//! - High-level entry points (`create`, `list`, `members`) that load the
//!   `AppConfig` from `~/.magi`, resolve the configured Redis URL, and print
//!   human-readable output. These back the corresponding `magi team` CLI
//!   subcommands.
//! - Lower-level `*_with_url` / `*_with_connection` helpers that take an
//!   explicit Redis URL or connection. These contain the actual Redis logic and
//!   are reused by other modules (for example, invite-based onboarding) and by
//!   tests that need to target a specific Redis instance.
//!
//! Membership data is stored across several Redis structures:
//! a global set of team names, a per-team hash of metadata, a per-team set of
//! agent names, a per-agent hash of profile fields, and a per-agent set of
//! `type:project` registration tuples.

use redis::AsyncCommands;

use crate::config::AppConfig;
use crate::error::{MagiError, Result};
use crate::model::RedisKeys;
use crate::redis_client;

/// A single agent's membership within a team, as reconstructed from Redis.
///
/// Each field mirrors data persisted under the per-agent Redis hash (see
/// `RedisKeys::agent`) plus the most recent registration tuple. Equality is
/// derived so members can be compared directly in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMember {
    /// The agent's unique name within the team (the Redis set member value).
    pub name: String,
    /// The agent's kind/category (for example, the CLI agent type), stored in
    /// the agent hash under the `type` field.
    pub agent_type: String,
    /// The project the agent is most recently registered against, parsed from
    /// the latest `type:project` registration tuple (empty when none exist).
    pub project: String,
    /// Unix-epoch-seconds timestamp (as a string) of when the agent was first
    /// registered.
    pub created_at: String,
    /// Unix-epoch-seconds timestamp (as a string) of the agent's most recent
    /// registration/heartbeat.
    pub last_seen_at: String,
}

/// Creates a new team and registers the current agent as its owner.
///
/// This is the entry point for the `magi team create` subcommand. It loads the
/// local `AppConfig`, resolves the configured Redis URL, and derives the
/// owner name from the active agent identity (falling back to `"owner"` when no
/// active agent is set). On success it prints a confirmation line.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded, if `redis.url` is not
/// configured, if the Redis connection fails, or if a team with the same name
/// already exists (see `create_team_with_url`).
pub async fn create(name: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    // Use the active agent identity as the team owner; default to "owner" when
    // no identity has been configured yet.
    let owner = config
        .identity
        .active_agent
        .as_deref()
        .unwrap_or("owner")
        .to_string();

    create_team_with_url(&url, &name, &owner).await?;
    println!("Created team: {name}");
    Ok(())
}

/// Lists every known team, one per line.
///
/// Backs the `magi team list` subcommand. Team names are read from the global
/// Redis set returned by `RedisKeys::teams`; an empty team prefix is used here
/// because that set is not scoped to any particular team.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded, if `redis.url` is not
/// configured, or if the Redis connection/read fails.
pub async fn list() -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let mut connection = redis_client::connect(&url).await?;
    // The global team-name set is not team-scoped, so an empty prefix is fine.
    let keys = RedisKeys::new("");
    let teams: Vec<String> = connection.smembers(keys.teams()).await?;

    for team in teams {
        println!("{team}");
    }

    Ok(())
}

/// Prints the members of a team along with a total count.
///
/// Backs the `magi team members` subcommand. When `name` is `None`, the team is
/// taken from the active team configured in the local identity. Each member is
/// printed as `name (type) - project`.
///
/// # Errors
///
/// Returns an error if the config cannot be loaded, if `redis.url` is not
/// configured, if neither an explicit `name` nor an active team is available
/// (`MagiError::InvalidConfig`), or if the Redis read fails.
pub async fn members(name: Option<String>) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    // Resolve the target team: explicit argument wins, otherwise fall back to
    // the active team from the configured identity.
    let team = name
        .or(config.identity.active_team)
        .ok_or_else(|| MagiError::InvalidConfig("team is required".to_string()))?;
    let members = list_members_with_url(&url, &team).await?;

    println!("Team: {team}");
    println!();
    for member in &members {
        println!(
            "  {} ({}) - {}",
            member.name, member.agent_type, member.project
        );
    }
    println!();
    println!("{} member(s)", members.len());

    Ok(())
}

/// Creates a team on a specific Redis instance and registers its `owner`.
///
/// This is the connection-explicit core used by `create` and by callers (such
/// as onboarding flows and tests) that already know the Redis URL. It claims the
/// team name, writes the team metadata hash, and registers the owner as the
/// first member, all guarded against partial failure.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` if a team named `team` already exists, and
/// propagates any Redis connection or command error. On a failed metadata write
/// the team-name claim is rolled back before the error is returned, so a later
/// attempt can recreate the team cleanly.
pub async fn create_team_with_url(url: &str, team: &str, owner: &str) -> Result<()> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;

    // Claim the team name atomically. SADD reports how many members were newly
    // added, so a return of 0 means the team already exists and we must not
    // overwrite its owner or timestamps.
    let added: i64 = connection.sadd(keys.teams(), team).await?;
    if added == 0 {
        return Err(MagiError::InvalidConfig(format!(
            "team `{team}` already exists"
        )));
    }

    let now = unix_timestamp_string();
    // Build a single MULTI/EXEC transaction so the team-metadata hash and the
    // owner's registration either all apply or none do.
    let mut pipe = redis::pipe();
    pipe.atomic()
        .hset(keys.team(), "name", team)
        .hset(keys.team(), "owner", owner)
        .hset(keys.team(), "created_at", &now)
        .hset(keys.team(), "updated_at", &now);
    // Register the owner with an "owner" type and no project; reuse the same
    // timestamp for both created_at and last_seen_at on first registration.
    add_agent_registration_to_pipe(&mut pipe, &keys, owner, "owner", "", &now, &now);

    let result: redis::RedisResult<()> = pipe.query_async(&mut connection).await;
    if let Err(error) = result {
        // Roll back the team-name claim so the partially created team can be
        // recreated by a later attempt.
        let _: redis::RedisResult<i64> = connection.srem(keys.teams(), team).await;
        return Err(error.into());
    }

    Ok(())
}

/// Registers (or refreshes) an agent's membership in a team via a URL.
///
/// Opens a Redis connection to `url` and delegates to
/// `register_agent_with_connection`. Used by onboarding/invite flows that
/// supply the team, agent name, agent type, and current project explicitly.
///
/// # Errors
///
/// Returns an error if the Redis connection fails or the underlying
/// registration pipeline fails.
pub async fn register_agent_with_url(
    url: &str,
    team: &str,
    agent: &str,
    agent_type: &str,
    project: &str,
) -> Result<()> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    register_agent_with_connection(&mut connection, &keys, agent, agent_type, project).await
}

/// Loads the full membership of a team from a specific Redis instance.
///
/// Reads the per-team agent set, then for each agent fetches its profile hash
/// (type and timestamps) and registration tuples, assembling a sorted list of
/// `TeamMember` values. Agents and registrations are sorted so output is
/// deterministic.
///
/// # Errors
///
/// Returns an error if the Redis connection fails or any of the per-agent reads
/// (hash fields or registration set) fail.
pub async fn list_members_with_url(url: &str, team: &str) -> Result<Vec<TeamMember>> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    let mut agents: Vec<String> = connection.smembers(keys.team_agents()).await?;
    // Sort for stable, alphabetical member ordering across runs.
    agents.sort();

    let mut members = Vec::with_capacity(agents.len());
    for agent in agents {
        let agent_key = keys.agent(&agent);
        // Fetch the three profile fields in a single round trip via a pipeline.
        let (agent_type, created_at, last_seen_at): (String, String, String) = redis::pipe()
            .hget(&agent_key, "type")
            .hget(&agent_key, "created_at")
            .hget(&agent_key, "last_seen_at")
            .query_async(&mut connection)
            .await?;
        let mut registrations: Vec<String> =
            connection.smembers(keys.registrations(&agent)).await?;
        // Sort so the "latest" registration picked below is deterministic.
        registrations.sort();
        // Each registration tuple is "type:project"; take the project portion
        // of the last (highest-sorted) tuple, defaulting to empty when none.
        let project = registrations
            .last()
            .and_then(|registration| registration.split_once(':').map(|(_, project)| project))
            .unwrap_or("")
            .to_string();

        members.push(TeamMember {
            name: agent,
            agent_type,
            project,
            created_at,
            last_seen_at,
        });
    }

    Ok(members)
}

/// Registers or refreshes an agent's membership using an existing connection.
///
/// This is the shared core of agent registration. It preserves the agent's
/// original `created_at` if it already exists (treating a re-registration as a
/// heartbeat) while updating `last_seen_at` to now, then applies the membership
/// writes as a single atomic transaction.
///
/// Crate-visible so other modules can register agents on a connection they
/// already hold without re-resolving the Redis URL.
///
/// # Errors
///
/// Returns an error if reading the existing `created_at` field fails or if the
/// registration transaction fails.
pub(crate) async fn register_agent_with_connection(
    connection: &mut redis::aio::MultiplexedConnection,
    keys: &RedisKeys,
    agent: &str,
    agent_type: &str,
    project: &str,
) -> Result<()> {
    let now = unix_timestamp_string();
    let agent_key = keys.agent(agent);

    // Preserve the first-seen timestamp on re-registration: reuse the stored
    // created_at when present, otherwise treat this as the agent's first sight.
    let created_at: Option<String> = connection.hget(&agent_key, "created_at").await?;
    let created_at = created_at.unwrap_or_else(|| now.clone());

    // Apply the membership writes atomically (MULTI/EXEC) so a partial update
    // cannot leave the agent half-registered.
    let mut pipe = redis::pipe();
    pipe.atomic();
    add_agent_registration_to_pipe(
        &mut pipe,
        keys,
        agent,
        agent_type,
        project,
        &created_at,
        &now,
    );

    let _: () = pipe.query_async(connection).await?;

    Ok(())
}

/// Appends the Redis commands that register an agent into `pipe`.
///
/// Queues the writes that add the agent to the team's agent set, populate its
/// profile hash (`name`, `type`, `created_at`, `last_seen_at`), and — when a
/// non-empty `project` is given — record a `type:project` registration tuple.
/// The caller is responsible for executing `pipe` (typically wrapped in
/// `atomic()`), which keeps this helper reusable across both team creation and
/// agent re-registration.
fn add_agent_registration_to_pipe(
    pipe: &mut redis::Pipeline,
    keys: &RedisKeys,
    agent: &str,
    agent_type: &str,
    project: &str,
    created_at: &str,
    last_seen_at: &str,
) {
    let agent_key = keys.agent(agent);
    // Add the agent to the team roster and write/overwrite its profile fields.
    pipe.sadd(keys.team_agents(), agent)
        .hset(&agent_key, "name", agent)
        .hset(&agent_key, "type", agent_type)
        .hset(&agent_key, "created_at", created_at)
        .hset(&agent_key, "last_seen_at", last_seen_at);

    // Only record a registration tuple when a project is supplied; the owner's
    // initial registration, for example, has no project.
    if !project.is_empty() {
        pipe.sadd(keys.registrations(agent), format!("{agent_type}:{project}"));
    }
}

/// Returns the current time as a Unix-epoch-seconds value rendered as a string.
///
/// Crate-visible so other modules can produce timestamps in the same format
/// used for the `created_at` / `last_seen_at` fields stored in Redis. If the
/// system clock is somehow before the Unix epoch, the duration defaults to zero
/// rather than panicking, yielding `"0"`.
pub(crate) fn unix_timestamp_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

/// Extracts the configured Redis URL from the loaded application config.
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` when `redis.url` is absent, signalling
/// that the user has not pointed `magi` at a Redis instance yet.
fn configured_redis_url(config: &AppConfig) -> Result<String> {
    config
        .redis
        .url
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("redis.url is not configured".to_string()))
}
