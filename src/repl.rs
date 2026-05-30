//! Interactive REPL (Read-Eval-Print Loop) for the magi CLI.
//!
//! This module implements the interactive command interface that users enter when
//! they run `magi` without a subcommand (or with a dedicated interactive flag).
//! It presents a `magi> ` prompt, reads one line at a time from stdin, parses the
//! line into a `ReplCommand`, dispatches it to the appropriate messaging or team
//! helper, and prints any error without terminating the session.
//!
//! The REPL is intentionally simple: no readline history, no tab-completion — just
//! a raw stdin loop.  Persistent message storage and team state live in Redis and
//! are managed by `crate::messaging` and `crate::team`.

use std::io::{self, Write};

use crate::error::{MagiError, Result};

/// All commands that can be issued from the interactive REPL prompt.
///
/// Each variant maps to a short, memorable keyword (or keyword + arguments)
/// that the user types at the `magi> ` prompt.  The variants are parsed by
/// `parse_repl_command` and dispatched in `run`.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ReplCommand {
    /// Display the agent's own inbox — messages addressed to the current agent.
    Inbox,
    /// Send a message to another agent.
    ///
    /// `to` is the recipient agent name; `body` is the message text.
    Send { to: String, body: String },
    /// Show conversation history, optionally filtered to a single peer `agent`.
    ///
    /// When `agent` is `None`, history for all peers is returned.
    History { agent: Option<String> },
    /// List all known team members visible to this agent.
    Team,
    /// Exit the REPL cleanly, returning `Ok(())` to the caller.
    Quit,
}

/// Run the interactive REPL until the user quits or stdin is exhausted.
///
/// Presents a `magi> ` prompt and processes one line per iteration.  Invalid
/// input and transient command errors are reported to stderr but do **not**
/// terminate the session — the loop continues so the user can retry.
///
/// # Returns
///
/// Returns `Ok(())` when the user types `quit`/`exit` or when stdin reaches
/// EOF (zero bytes read).
///
/// # Errors
///
/// Returns `Err` only if flushing stdout or reading from stdin fails at the
/// OS level — conditions from which recovery is not practical.
pub async fn run() -> Result<()> {
    println!("magi interactive mode. Commands: inbox, send <agent> <message>, history [agent], team, quit");

    loop {
        // Write the prompt without a trailing newline so the cursor stays on
        // the same line as the user's input.
        print!("magi> ");
        // Flush stdout explicitly because it is line-buffered by default; the
        // prompt would otherwise only appear after the user presses Enter.
        io::stdout().flush()?;

        let mut line = String::new();
        // A return value of 0 means EOF — stdin was closed or the user pressed
        // Ctrl-D; treat it as a clean exit.
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
            // Return immediately; the loop terminates cleanly.
            ReplCommand::Quit => return Ok(()),
        };

        if let Err(error) = result {
            // Surface transient command failures without terminating the session.
            eprintln!("{error}");
        }
    }
}

/// Parse a single line of user input into a `ReplCommand`.
///
/// Whitespace (including the trailing newline added by `read_line`) is trimmed
/// before matching.  An empty line is treated the same as `inbox` so that
/// pressing Enter alone refreshes the inbox — a common interactive pattern.
///
/// # Supported syntax
///
/// ```text
/// inbox                  → ReplCommand::Inbox  (also matches empty line)
/// team                   → ReplCommand::Team
/// quit | exit            → ReplCommand::Quit
/// history                → ReplCommand::History { agent: None }
/// history <agent>        → ReplCommand::History { agent: Some(...) }
/// send <agent> <message> → ReplCommand::Send { to, body }
/// ```
///
/// # Errors
///
/// Returns `MagiError::InvalidConfig` when:
/// - `history` is followed by more than one token (ambiguous agent name).
/// - `send` is missing the recipient, the message body, or either is empty.
/// - The input does not match any recognised command.
pub fn parse_repl_command(input: &str) -> Result<ReplCommand> {
    // Trim surrounding whitespace (including the '\n' from read_line).
    let input = input.trim();
    // Treat an empty line as "inbox" for a fast refresh workflow.
    if input.is_empty() || input == "inbox" {
        return Ok(ReplCommand::Inbox);
    }

    if input == "team" {
        return Ok(ReplCommand::Team);
    }

    // Accept both "quit" and the more conventional "exit" alias.
    if input == "quit" || input == "exit" {
        return Ok(ReplCommand::Quit);
    }

    // Bare "history" without an agent name returns the full conversation log.
    if input == "history" {
        return Ok(ReplCommand::History { agent: None });
    }

    // "history <agent>" — strip the prefix and validate exactly one token.
    if let Some(agent) = input.strip_prefix("history ") {
        let agent = agent.trim();
        // Reject empty strings and multi-word inputs (e.g. "history foo bar").
        if agent.is_empty() || agent.split_whitespace().count() != 1 {
            return Err(MagiError::InvalidConfig(
                "usage: history [agent]".to_string(),
            ));
        }
        return Ok(ReplCommand::History {
            agent: Some(agent.to_string()),
        });
    }

    // "send <agent> <message>" — split on the first whitespace character to
    // allow the message body to contain spaces.
    if let Some(rest) = input.strip_prefix("send ") {
        let Some((to, body)) = rest.trim().split_once(char::is_whitespace) else {
            return Err(MagiError::InvalidConfig(
                "usage: send <agent> <message>".to_string(),
            ));
        };
        let to = to.trim();
        let body = body.trim();
        // Guard against degenerate input where either token is blank after trimming.
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

    // Nothing matched — report the unrecognised input verbatim so the user
    // knows exactly what was rejected.
    Err(MagiError::InvalidConfig(format!(
        "unknown interactive command `{input}`"
    )))
}
