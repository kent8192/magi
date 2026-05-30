//! Low-level Redis connection helpers used throughout the magi CLI.
//!
//! This module provides thin wrappers around the `redis` crate's async API,
//! establishing and validating connections to whatever Redis instance magi is
//! configured to use — whether that is the embedded managed server started by
//! the `magi server` sub-command or an externally supplied Redis URL.
//!
//! All functions accept a Redis URL string (e.g. `redis://127.0.0.1:6379`) and
//! return a `Result` defined in `crate::error`.  Higher-level modules such as
//! the messaging layer, watch mode, and the REPL depend on these primitives
//! rather than constructing raw `redis::Client` instances themselves.

use crate::error::Result;

/// Opens a multiplexed async connection to the Redis server at `url`.
///
/// A `redis::aio::MultiplexedConnection` allows multiple in-flight commands
/// to share a single TCP connection, which is appropriate for the async tasks
/// that magi runs concurrently (e.g. stream readers and Pub/Sub listeners).
///
/// # Errors
///
/// Returns an error if the URL is invalid or if the underlying TCP connection
/// to the Redis server cannot be established.
pub async fn connect(url: &str) -> Result<redis::aio::MultiplexedConnection> {
    let client = redis::Client::open(url)?;
    let connection = client.get_multiplexed_async_connection().await?;
    Ok(connection)
}

/// Sends a `PING` command to the Redis server at `url` and validates the reply.
///
/// This is used during startup and health-check sequences to confirm that the
/// Redis server (embedded or external) is reachable and responding correctly
/// before magi attempts to use it for messaging or stream operations.
///
/// Internally this calls `connect` and then `validate_ping_response`, so
/// the connection is not retained after the function returns.
///
/// # Errors
///
/// Returns an error if the connection cannot be established, if the `PING`
/// command fails at the network level, or if the server returns an unexpected
/// response (see `validate_ping_response`).
pub async fn ping(url: &str) -> Result<()> {
    let mut connection = connect(url).await?;
    let response: String = redis::cmd("PING").query_async(&mut connection).await?;
    validate_ping_response(&response)
}

/// Checks that a Redis `PING` response is one of the accepted success values.
///
/// Redis normally replies with `"PONG"`, but some configurations (e.g. servers
/// protected by `AUTH` before the `PING` is acknowledged) may reply `"OK"`.
/// The comparison is case-insensitive to accommodate non-standard casing that
/// some Redis proxies or test stubs may produce.
///
/// # Errors
///
/// Returns a `redis::RedisError` with kind
/// `redis::ErrorKind::UnexpectedReturnType` when the reply is neither `PONG`
/// nor `OK`.
fn validate_ping_response(response: &str) -> Result<()> {
    if response.eq_ignore_ascii_case("PONG") || response.eq_ignore_ascii_case("OK") {
        return Ok(());
    }

    // Neither accepted value matched — surface the raw reply so callers can
    // include it in diagnostics without a separate Redis lookup.
    Err(redis::RedisError::from((
        redis::ErrorKind::UnexpectedReturnType,
        "unexpected PING response",
        response.to_string(),
    ))
    .into())
}

/// Publishes `message` to a Redis Pub/Sub `channel` using the server at `url`.
///
/// This is a fire-and-forget helper: the return value of the Redis `PUBLISH`
/// command (the number of subscribers that received the message) is intentionally
/// discarded.  Callers that need delivery guarantees should use Redis Streams
/// (via the messaging layer) rather than raw Pub/Sub.
///
/// A fresh connection is opened for each call; for high-frequency publishing
/// callers should prefer to reuse a connection obtained from `connect`.
///
/// # Errors
///
/// Returns an error if the connection cannot be established or if the `PUBLISH`
/// command fails at the Redis protocol level.
pub async fn publish(url: &str, channel: &str, message: &str) -> Result<()> {
    let mut connection = connect(url).await?;
    // The usize return is the subscriber count; magi does not use it.
    let _: usize = redis::cmd("PUBLISH")
        .arg(channel)
        .arg(message)
        .query_async(&mut connection)
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_ping_response;

    #[test]
    fn validate_ping_response_accepts_pong_and_ok_case_insensitively() {
        assert!(validate_ping_response("PONG").is_ok());
        assert!(validate_ping_response("pong").is_ok());
        assert!(validate_ping_response("OK").is_ok());
        assert!(validate_ping_response("ok").is_ok());
    }

    #[test]
    fn validate_ping_response_rejects_unexpected_response() {
        assert!(validate_ping_response("NOPE").is_err());
    }
}
