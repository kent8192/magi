use std::time::Duration;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use redis::AsyncCommands;
use sha2::{Digest, Sha256};

use crate::config::AppConfig;
use crate::error::{MagiError, Result};
use crate::model::RedisKeys;
use crate::redis_client;
use crate::team;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedInvite {
    pub invite_id: String,
    pub token: String,
    pub token_hash: String,
    pub team: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteSummary {
    pub invite_id: String,
    pub team: String,
    pub created_by: String,
    pub created_at: String,
    pub expires_at: String,
    pub used_count: u64,
    pub max_uses: u64,
    pub revoked_at: Option<String>,
}

type RawInviteSummary = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<u64>,
    Option<u64>,
    Option<String>,
);

pub async fn create(team: String, ttl: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let created_by = config
        .identity
        .active_agent
        .as_deref()
        .unwrap_or("owner")
        .to_string();
    let ttl = parse_ttl(&ttl)?;
    let invite = create_invite_with_url(&url, &team, &created_by, ttl).await?;

    println!("Invite: {}", invite.token);
    Ok(())
}

pub async fn list(team: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let invites = list_invites_with_url(&url, &team).await?;

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

pub async fn revoke(invite_id: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let team = config
        .identity
        .active_team
        .ok_or_else(|| MagiError::InvalidConfig("identity.active_team is required".to_string()))?;

    revoke_invite_with_url(&url, &team, &invite_id).await?;
    println!("Revoked invite: {invite_id}");
    Ok(())
}

pub async fn join(invite: String) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let agent = config
        .identity
        .active_agent
        .as_deref()
        .unwrap_or("agent")
        .to_string();
    let project = std::env::current_dir()?.display().to_string();

    let team = join_with_url(&url, &invite, &agent, "codex", &project).await?;
    println!("Joined team {team} as {agent}");
    Ok(())
}

pub async fn create_invite_with_url(
    url: &str,
    team: &str,
    created_by: &str,
    ttl: Duration,
) -> Result<CreatedInvite> {
    if ttl.is_zero() {
        return Err(MagiError::InvalidConfig(
            "invite ttl must be greater than zero".to_string(),
        ));
    }

    let keys = RedisKeys::new(team);
    let invite_id = format!("inv_{}", random_urlsafe(16));
    let token = random_urlsafe(32);
    let token_hash = token_hash(&token);
    let lookup_key = RedisKeys::invite_token(&token_hash);
    let now = now_secs();
    let expires_at = now + ttl.as_secs();
    let mut connection = redis_client::connect(url).await?;

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

pub async fn list_invites_with_url(url: &str, team: &str) -> Result<Vec<InviteSummary>> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    let mut invite_ids: Vec<String> = connection
        .smembers(format!("{}:invites", keys.team()))
        .await?;
    invite_ids.sort();

    let mut invites = Vec::with_capacity(invite_ids.len());
    for invite_id in invite_ids {
        let invite_key = keys.invite(&invite_id);
        let exists: bool = connection.exists(&invite_key).await?;
        if !exists {
            continue;
        }

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

        if stored_team.as_deref() != Some(team) {
            continue;
        }

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

pub async fn join_with_url(
    url: &str,
    token: &str,
    agent: &str,
    agent_type: &str,
    project: &str,
) -> Result<String> {
    let hash = token_hash(token);
    let lookup_key = RedisKeys::invite_token(&hash);
    let mut connection = redis_client::connect(url).await?;
    let consumed = consume_invite_atomically(&mut connection, &lookup_key, &hash).await?;
    let keys = RedisKeys::new(&consumed.team);

    if let Err(error) =
        team::register_agent_with_connection(&mut connection, &keys, agent, agent_type, project)
            .await
    {
        let _: std::result::Result<i64, redis::RedisError> = connection
            .hincr(&consumed.invite_key, "used_count", -1)
            .await;
        return Err(error);
    }

    Ok(consumed.team)
}

pub async fn revoke_invite_with_url(url: &str, team: &str, invite_id: &str) -> Result<()> {
    let keys = RedisKeys::new(team);
    let invite_key = keys.invite(invite_id);
    let mut connection = redis_client::connect(url).await?;
    let (stored_team, token_hash): (Option<String>, Option<String>) = redis::pipe()
        .hget(&invite_key, "team")
        .hget(&invite_key, "token_hash")
        .query_async(&mut connection)
        .await?;
    if stored_team.as_deref() != Some(team) {
        return Err(MagiError::NotFound(format!("invite `{invite_id}`")));
    }
    let token_hash =
        token_hash.ok_or_else(|| MagiError::NotFound(format!("invite `{invite_id}`")))?;

    let lookup_key = RedisKeys::invite_token(&token_hash);
    let _: () = redis::pipe()
        .atomic()
        .hset(&invite_key, "revoked_at", now_secs())
        .del(lookup_key)
        .query_async(&mut connection)
        .await?;

    Ok(())
}

pub fn parse_ttl(value: &str) -> Result<Duration> {
    let Some((number, suffix)) = value.split_at_checked(value.len().saturating_sub(1)) else {
        return Err(MagiError::InvalidConfig(
            "invite ttl is invalid".to_string(),
        ));
    };
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

pub fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

struct ConsumedInvite {
    team: String,
    invite_key: String,
}

async fn consume_invite_atomically(
    connection: &mut redis::aio::MultiplexedConnection,
    lookup_key: &str,
    expected_hash: &str,
) -> Result<ConsumedInvite> {
    let response: Vec<String> = redis::Script::new(
        r#"
local invite_key = redis.call("GET", KEYS[1])
if not invite_key then
  return {"invalid"}
end

if redis.call("EXISTS", invite_key) == 0 then
  redis.call("DEL", KEYS[1])
  return {"invalid"}
end

local stored_hash = redis.call("HGET", invite_key, "token_hash")
if stored_hash ~= ARGV[1] then
  return {"invalid"}
end

local revoked_at = redis.call("HGET", invite_key, "revoked_at")
if revoked_at and revoked_at ~= "" then
  return {"revoked"}
end

local expires_at = tonumber(redis.call("HGET", invite_key, "expires_at"))
if not expires_at then
  return {"invalid"}
end

if expires_at <= tonumber(ARGV[2]) then
  redis.call("DEL", KEYS[1])
  return {"expired"}
end

local used_count = tonumber(redis.call("HGET", invite_key, "used_count") or "0")
local max_uses = tonumber(redis.call("HGET", invite_key, "max_uses") or "0")
if max_uses > 0 and used_count >= max_uses then
  return {"max_uses"}
end

redis.call("HINCRBY", invite_key, "used_count", 1)
return {"ok", redis.call("HGET", invite_key, "team"), invite_key}
"#,
    )
    .key(lookup_key)
    .arg(expected_hash)
    .arg(now_secs())
    .invoke_async(connection)
    .await?;

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

fn random_urlsafe(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    for byte in &mut buffer {
        *byte = rand::random();
    }
    URL_SAFE_NO_PAD.encode(buffer)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn configured_redis_url(config: &AppConfig) -> Result<String> {
    config
        .redis
        .url
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("redis.url is not configured".to_string()))
}
