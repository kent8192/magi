use serde::{Deserialize, Serialize};

pub const REDIS_KEY_PREFIX: &str = "magi";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedisKeys {
    team_id: String,
}

impl RedisKeys {
    pub fn new(team_id: impl Into<String>) -> Self {
        Self {
            team_id: encode_key_segment(&team_id.into()),
        }
    }

    pub fn teams(&self) -> String {
        format!("{REDIS_KEY_PREFIX}:teams")
    }

    pub fn team(&self) -> String {
        format!("{REDIS_KEY_PREFIX}:team:{}", self.team_id)
    }

    pub fn team_agents(&self) -> String {
        format!("{}:agents", self.team())
    }

    pub fn agent(&self, agent_id: &str) -> String {
        format!(
            "{REDIS_KEY_PREFIX}:agent:{}:{}",
            self.team_id,
            encode_key_segment(agent_id)
        )
    }

    pub fn registrations(&self, agent_id: &str) -> String {
        format!("{}:registrations", self.agent(agent_id))
    }

    pub fn stream(&self) -> String {
        format!("{REDIS_KEY_PREFIX}:stream:{}", self.team_id)
    }

    pub fn cursor(&self, agent_id: &str) -> String {
        format!(
            "{REDIS_KEY_PREFIX}:cursor:{}:{}",
            self.team_id,
            encode_key_segment(agent_id)
        )
    }

    pub fn pubsub(&self) -> String {
        format!("{REDIS_KEY_PREFIX}:pubsub:{}", self.team_id)
    }

    pub fn invite(&self, invite_id: &str) -> String {
        format!(
            "{REDIS_KEY_PREFIX}:invite:{}",
            encode_key_segment(invite_id)
        )
    }

    pub fn invite_token(token_hash: &str) -> String {
        format!(
            "{REDIS_KEY_PREFIX}:invite_token:{}",
            encode_key_segment(token_hash)
        )
    }
}

fn encode_key_segment(segment: &str) -> String {
    let mut encoded = String::with_capacity(segment.len());

    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => encoded.push(byte as char),
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }

    encoded
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentIdentity {
    pub name: String,
    pub team: String,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct MessageEvent {
    pub from: String,
    pub to: String,
    pub body: String,
    pub created_at: String,
}
