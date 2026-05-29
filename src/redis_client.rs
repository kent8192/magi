use crate::error::Result;

pub async fn connect(url: &str) -> Result<redis::aio::MultiplexedConnection> {
    let client = redis::Client::open(url)?;
    let connection = client.get_multiplexed_async_connection().await?;
    Ok(connection)
}

pub async fn ping(url: &str) -> Result<()> {
    let mut connection = connect(url).await?;
    let response: String = redis::cmd("PING").query_async(&mut connection).await?;
    validate_ping_response(&response)
}

fn validate_ping_response(response: &str) -> Result<()> {
    if response.eq_ignore_ascii_case("PONG") || response.eq_ignore_ascii_case("OK") {
        return Ok(());
    }

    Err(redis::RedisError::from((
        redis::ErrorKind::UnexpectedReturnType,
        "unexpected PING response",
        response.to_string(),
    ))
    .into())
}

pub async fn publish(url: &str, channel: &str, message: &str) -> Result<()> {
    let mut connection = connect(url).await?;
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
