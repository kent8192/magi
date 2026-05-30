use redis::AsyncCommands;

use crate::config::AppConfig;
use crate::error::{MagiError, Result};
use crate::model::RedisKeys;
use crate::redis_client;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamMember {
    pub name: String,
    pub agent_type: String,
    pub project: String,
    pub created_at: String,
    pub last_seen_at: String,
}

pub async fn create(name: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
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

pub async fn list() -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let mut connection = redis_client::connect(&url).await?;
    let keys = RedisKeys::new("");
    let teams: Vec<String> = connection.smembers(keys.teams()).await?;

    for team in teams {
        println!("{team}");
    }

    Ok(())
}

pub async fn members(name: Option<String>) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
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
    let mut pipe = redis::pipe();
    pipe.atomic()
        .hset(keys.team(), "name", team)
        .hset(keys.team(), "owner", owner)
        .hset(keys.team(), "created_at", &now)
        .hset(keys.team(), "updated_at", &now);
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

pub async fn list_members_with_url(url: &str, team: &str) -> Result<Vec<TeamMember>> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    let mut agents: Vec<String> = connection.smembers(keys.team_agents()).await?;
    agents.sort();

    let mut members = Vec::with_capacity(agents.len());
    for agent in agents {
        let agent_key = keys.agent(&agent);
        let (agent_type, created_at, last_seen_at): (String, String, String) = redis::pipe()
            .hget(&agent_key, "type")
            .hget(&agent_key, "created_at")
            .hget(&agent_key, "last_seen_at")
            .query_async(&mut connection)
            .await?;
        let mut registrations: Vec<String> =
            connection.smembers(keys.registrations(&agent)).await?;
        registrations.sort();
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

pub(crate) async fn register_agent_with_connection(
    connection: &mut redis::aio::MultiplexedConnection,
    keys: &RedisKeys,
    agent: &str,
    agent_type: &str,
    project: &str,
) -> Result<()> {
    let now = unix_timestamp_string();
    let agent_key = keys.agent(agent);

    let created_at: Option<String> = connection.hget(&agent_key, "created_at").await?;
    let created_at = created_at.unwrap_or_else(|| now.clone());

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
    pipe.sadd(keys.team_agents(), agent)
        .hset(&agent_key, "name", agent)
        .hset(&agent_key, "type", agent_type)
        .hset(&agent_key, "created_at", created_at)
        .hset(&agent_key, "last_seen_at", last_seen_at);

    if !project.is_empty() {
        pipe.sadd(keys.registrations(agent), format!("{agent_type}:{project}"));
    }
}

pub(crate) fn unix_timestamp_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn configured_redis_url(config: &AppConfig) -> Result<String> {
    config
        .redis
        .url
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("redis.url is not configured".to_string()))
}
