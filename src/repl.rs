use std::io::{self, Write};

use crate::error::{MagiError, Result};

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReplCommand {
    Inbox,
    Send { to: String, body: String },
    History { agent: Option<String> },
    Team,
    Quit,
}

pub async fn run() -> Result<()> {
    println!("magi interactive mode. Commands: inbox, send <agent> <message>, history [agent], team, quit");

    loop {
        print!("magi> ");
        io::stdout().flush()?;

        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            return Ok(());
        }

        let command = match parse_repl_command(&line) {
            Ok(command) => command,
            Err(error) => {
                // Keep the session alive on invalid input instead of exiting.
                eprintln!("{error}");
                continue;
            }
        };

        let result = match command {
            ReplCommand::Inbox => crate::messaging::inbox().await,
            ReplCommand::Send { to, body } => crate::messaging::send(to, vec![body]).await,
            ReplCommand::History { agent } => crate::messaging::history(None, agent).await,
            ReplCommand::Team => crate::team::members(None).await,
            ReplCommand::Quit => return Ok(()),
        };

        if let Err(error) = result {
            // Surface transient command failures without terminating the session.
            eprintln!("{error}");
        }
    }
}

pub fn parse_repl_command(input: &str) -> Result<ReplCommand> {
    let input = input.trim();
    if input.is_empty() || input == "inbox" {
        return Ok(ReplCommand::Inbox);
    }

    if input == "team" {
        return Ok(ReplCommand::Team);
    }

    if input == "quit" || input == "exit" {
        return Ok(ReplCommand::Quit);
    }

    if input == "history" {
        return Ok(ReplCommand::History { agent: None });
    }

    if let Some(agent) = input.strip_prefix("history ") {
        let agent = agent.trim();
        if agent.is_empty() || agent.split_whitespace().count() != 1 {
            return Err(MagiError::InvalidConfig(
                "usage: history [agent]".to_string(),
            ));
        }
        return Ok(ReplCommand::History {
            agent: Some(agent.to_string()),
        });
    }

    if let Some(rest) = input.strip_prefix("send ") {
        let Some((to, body)) = rest.trim().split_once(char::is_whitespace) else {
            return Err(MagiError::InvalidConfig(
                "usage: send <agent> <message>".to_string(),
            ));
        };
        let to = to.trim();
        let body = body.trim();
        if to.is_empty() || body.is_empty() {
            return Err(MagiError::InvalidConfig(
                "usage: send <agent> <message>".to_string(),
            ));
        }
        return Ok(ReplCommand::Send {
            to: to.to_string(),
            body: body.to_string(),
        });
    }

    Err(MagiError::InvalidConfig(format!(
        "unknown interactive command `{input}`"
    )))
}
