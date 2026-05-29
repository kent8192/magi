use thiserror::Error;

pub type Result<T> = std::result::Result<T, MagiError>;

#[derive(Debug, Error)]
pub enum MagiError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("toml deserialize error: {0}")]
    TomlDeserialize(#[from] toml::de::Error),

    #[error("toml serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("command failed: {0}")]
    CommandFailed(String),
}
