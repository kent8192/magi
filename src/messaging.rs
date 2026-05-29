use redis::AsyncCommands;

use crate::config::AppConfig;
use crate::error::{MagiError, Result};
use crate::model::{MessageEvent, RedisKeys};
use crate::redis_client;
use crate::team;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum InboxReadMode {
    MarkRead,
    Peek,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct MessageRecord {
    pub id: String,
    pub event: MessageEvent,
}

pub async fn send(to: String, message: Vec<String>) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let team = active_team(&config)?;
    let from = active_agent(&config)?;
    let body = message.join(" ");

    let record = send_message_with_url(&url, &team, &from, &to, &body).await?;
    println!(
        "sent {} {} -> {}",
        record.id, record.event.from, record.event.to
    );
    Ok(())
}

pub async fn inbox() -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let team = active_team(&config)?;
    let agent = active_agent(&config)?;

    for message in read_inbox_with_url(&url, &team, &agent, InboxReadMode::MarkRead).await? {
        println!("{}", format_message_line(&message));
    }
    Ok(())
}

pub async fn history(team: Option<String>, agent: Option<String>) -> Result<()> {
    let config = AppConfig::load()?;
    let url = configured_redis_url(&config)?;
    let team = team
        .or(config.identity.active_team)
        .ok_or_else(|| MagiError::InvalidConfig("identity.active_team is required".to_string()))?;

    for message in history_with_url(&url, &team, agent.as_deref()).await? {
        println!("{}", format_message_line(&message));
    }
    Ok(())
}

pub async fn send_message_with_url(
    url: &str,
    team: &str,
    from: &str,
    to: &str,
    body: &str,
) -> Result<MessageRecord> {
    let body = body.trim();
    if body.is_empty() {
        return Err(MagiError::InvalidConfig(
            "message body must not be empty".to_string(),
        ));
    }

    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    ensure_agent_exists(&mut connection, &keys, from, "sender").await?;
    ensure_agent_exists(&mut connection, &keys, to, "recipient").await?;

    let event = MessageEvent {
        from: from.to_string(),
        to: to.to_string(),
        body: body.to_string(),
        created_at: team::unix_timestamp_string(),
    };

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

    let _: usize = connection.publish(keys.pubsub(), &id).await?;

    Ok(MessageRecord { id, event })
}

pub async fn read_inbox_with_url(
    url: &str,
    team: &str,
    agent: &str,
    mode: InboxReadMode,
) -> Result<Vec<MessageRecord>> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    ensure_agent_exists(&mut connection, &keys, agent, "recipient").await?;

    let cursor: Option<String> = connection.get(keys.cursor(agent)).await?;
    let cursor = cursor.unwrap_or_else(|| "0-0".to_string());
    let start = format!("({cursor}");
    let entries = stream_range(&mut connection, &keys.stream(), &start, "+").await?;
    let last_seen_id = entries.last().map(|message| message.id.clone());
    let messages = entries
        .into_iter()
        .filter(|message| message.event.to == agent)
        .collect::<Vec<_>>();

    if mode == InboxReadMode::MarkRead {
        if let Some(last_seen_id) = last_seen_id {
            let _: () = connection.set(keys.cursor(agent), last_seen_id).await?;
        }
    }

    Ok(messages)
}

pub async fn history_with_url(
    url: &str,
    team: &str,
    agent: Option<&str>,
) -> Result<Vec<MessageRecord>> {
    let keys = RedisKeys::new(team);
    let mut connection = redis_client::connect(url).await?;
    let mut messages = stream_range(&mut connection, &keys.stream(), "-", "+").await?;

    if let Some(agent) = agent {
        messages.retain(|message| message.event.from == agent || message.event.to == agent);
    }

    Ok(messages)
}

pub fn format_message_line(message: &MessageRecord) -> String {
    format!(
        "[{}] {} -> {}: {}",
        message.event.created_at, message.event.from, message.event.to, message.event.body
    )
}

async fn ensure_agent_exists(
    connection: &mut redis::aio::MultiplexedConnection,
    keys: &RedisKeys,
    agent: &str,
    role: &str,
) -> Result<()> {
    let exists: bool = connection.sismember(keys.team_agents(), agent).await?;
    if !exists {
        return Err(MagiError::NotFound(format!("{role} `{agent}`")));
    }
    Ok(())
}

async fn stream_range(
    connection: &mut redis::aio::MultiplexedConnection,
    stream: &str,
    start: &str,
    end: &str,
) -> Result<Vec<MessageRecord>> {
    let entries: Vec<(String, Vec<(String, String)>)> = redis::cmd("XRANGE")
        .arg(stream)
        .arg(start)
        .arg(end)
        .query_async(connection)
        .await?;

    entries
        .into_iter()
        .map(|(id, fields)| message_from_stream_fields(id, fields))
        .collect()
}

fn message_from_stream_fields(id: String, fields: Vec<(String, String)>) -> Result<MessageRecord> {
    let mut from = None;
    let mut to = None;
    let mut body = None;
    let mut created_at = None;

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

fn configured_redis_url(config: &AppConfig) -> Result<String> {
    config
        .redis
        .url
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("redis.url is not configured".to_string()))
}

fn active_team(config: &AppConfig) -> Result<String> {
    config
        .identity
        .active_team
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("identity.active_team is required".to_string()))
}

fn active_agent(config: &AppConfig) -> Result<String> {
    config
        .identity
        .active_agent
        .clone()
        .ok_or_else(|| MagiError::InvalidConfig("identity.active_agent is required".to_string()))
}
