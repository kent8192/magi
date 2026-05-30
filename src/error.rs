//! Crate-wide error type and `Result` alias for `magi`.
//!
//! All fallible operations in `magi` return `Result<T>`, which is a type
//! alias for `std::result::Result<T, MagiError>`.  `MagiError` collects
//! every category of failure that can arise across the CLI: I/O, Redis
//! connectivity, TOML configuration parsing/serialization, bad configuration
//! values, missing resources, and subprocess failures.
//!
//! Conversions from the underlying library error types are derived
//! automatically via `#[from]` attributes on the enum variants, so callers
//! can use the `?` operator directly without manual `map_err` calls.

use thiserror::Error;

/// Convenience alias that fixes the error type to `MagiError`.
///
/// Using this alias throughout the crate avoids repeating the error type at
/// every call site and makes it straightforward to change the error type in
/// the future if needed.
pub type Result<T> = std::result::Result<T, MagiError>;

/// The top-level error type for all `magi` operations.
///
/// Each variant represents a distinct failure category.  Variants that wrap a
/// foreign error type implement `From` automatically (via `#[from]`), enabling
/// the `?` operator to convert those errors at the point of use.
#[derive(Debug, Error)]
pub enum MagiError {
    /// An OS-level I/O failure, such as a missing file, a permission error, or
    /// a broken pipe.  Automatically converted from `std::io::Error`.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A failure originating from the Redis client library, such as a
    /// connection refused, an authentication error, or a protocol violation.
    /// Automatically converted from `redis::RedisError`.
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),

    /// A failure while deserializing a TOML configuration file into a typed
    /// structure.  Automatically converted from `toml::de::Error`.
    #[error("toml deserialize error: {0}")]
    TomlDeserialize(#[from] toml::de::Error),

    /// A failure while serializing a typed structure back to TOML, for example
    /// when writing a state or configuration file to disk.  Automatically
    /// converted from `toml::ser::Error`.
    #[error("toml serialize error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),

    /// The loaded configuration is structurally valid TOML but semantically
    /// invalid for `magi` (e.g., a required field is empty or a value is out
    /// of range).  The inner `String` carries a human-readable explanation.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// A required resource — such as a Redis key, a team member record, or a
    /// configuration entry — could not be located.  The inner `String`
    /// identifies what was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// An external process or subcommand (e.g., an SSH helper or an installer
    /// step) exited with a non-zero status or produced unexpected output.  The
    /// inner `String` carries the failure description.
    #[error("command failed: {0}")]
    CommandFailed(String),
}
