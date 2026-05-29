use futures_util::StreamExt;
use serde_json::json;

use crate::cli::WatchFormat;
use crate::config::AppConfig;
use crate::error::{MagiError, Result};
use crate::messaging::{self, InboxReadMode, MessageRecord};
use crate::model::RedisKeys;

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

async fn watch_loop_with_url(
    url: &str,
    team: &str,
    agent: &str,
    format: WatchFormat,
) -> Result<()> {
    let client = redis::Client::open(url)?;
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.subscribe(RedisKeys::new(team).pubsub()).await?;
    let mut wakeups = pubsub.on_message();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = interval.tick() => {
                print_new_messages(url, team, agent, format).await?;
            }
            Some(_) = wakeups.next() => {
                print_new_messages(url, team, agent, format).await?;
            }
        }
    }
}

async fn print_new_messages(url: &str, team: &str, agent: &str, format: WatchFormat) -> Result<()> {
    for line in watch_once_with_url(url, team, agent, format).await? {
        println!("{line}");
    }
    Ok(())
}
