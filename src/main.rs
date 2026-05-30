//! Binary entry point for the `magi` CLI.
//!
//! Parses the command-line arguments via `clap` and dispatches to the
//! appropriate handler in the `magi` library crate.  When no sub-command is
//! provided the interactive REPL is started, which lets an agent send and
//! receive messages in a conversational loop.
//!
//! # Dispatch overview
//!
//! ```text
//! magi                         → repl::run()          (interactive REPL)
//! magi redis {start|status|stop}   → redis_manager::*
//! magi team  {create|list|members} → team::*
//! magi invite {create|list|revoke} → invite::*
//! magi join <invite>           → invite::join()
//! magi send <to> <message>     → messaging::send()
//! magi inbox                   → messaging::inbox()
//! magi history                 → messaging::history()
//! magi watch                   → watch::run()
//! magi ssh  {start|status|stop}    → ssh::*
//! magi install                 → install::run()
//! magi config {get|set}        → config::*
//! ```
use clap::Parser;
use magi::cli::{
    Cli, Command, ConfigCommand, InviteCommand, RedisCommand, SshCommand, TeamCommand,
};
use magi::error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize a human-readable tracing subscriber for log output.
    tracing_subscriber::fmt::init();

    // Parse argv using the Clap-derived `Cli` struct.
    let cli = Cli::parse();
    match cli.command {
        // No sub-command: drop into the interactive REPL.
        None => magi::repl::run().await,

        // --- Embedded Redis server lifecycle ---
        Some(Command::Redis { command }) => match command {
            // Start the managed Redis server, optionally binding to LAN or a custom address.
            RedisCommand::Start { lan, bind } => magi::redis_manager::start(lan, bind).await,
            // Print the current Redis server status (running/stopped, PID, address).
            RedisCommand::Status => magi::redis_manager::status().await,
            // Gracefully stop the managed Redis server.
            RedisCommand::Stop => magi::redis_manager::stop().await,
        },

        // --- Team management ---
        Some(Command::Team { command }) => match command {
            // Create a new named team in Redis.
            TeamCommand::Create { name } => magi::team::create(name).await,
            // List all teams the current agent belongs to.
            TeamCommand::List => magi::team::list().await,
            // List members of a specific team.
            TeamCommand::Members { team } => magi::team::members(team).await,
        },

        // --- Invite-based onboarding ---
        Some(Command::Invite { command }) => match command {
            // Generate a time-limited invite token for a team.
            InviteCommand::Create { team, ttl } => magi::invite::create(team, ttl).await,
            // Show all active invites for a team.
            InviteCommand::List { team } => magi::invite::list(team).await,
            // Revoke an invite by its ID before it is accepted or expires.
            InviteCommand::Revoke { invite_id } => magi::invite::revoke(invite_id).await,
        },

        // Accept an invite and register the current agent as a team member.
        Some(Command::Join { invite }) => magi::invite::join(invite).await,

        // --- Messaging ---
        // Send a message to another agent or broadcast to a team.
        Some(Command::Send { to, message }) => magi::messaging::send(to, message).await,
        // Display unread messages from the agent's personal Redis Stream inbox.
        Some(Command::Inbox) => magi::messaging::inbox().await,
        // Show the message history for a team or a specific agent.
        Some(Command::History { team, agent }) => magi::messaging::history(team, agent).await,

        // Subscribe to the Pub/Sub channel and stream new messages to stdout
        // in either line or JSON format (controlled by `format`).
        Some(Command::Watch { format }) => magi::watch::run(format).await,

        // --- SSH helper lifecycle ---
        Some(Command::Ssh { command }) => match command {
            // Start the SSH helper service (allows remote agents to connect).
            SshCommand::Start => magi::ssh::start().await,
            // Print the current SSH helper status.
            SshCommand::Status => magi::ssh::status().await,
            // Stop the SSH helper service.
            SshCommand::Stop => magi::ssh::stop().await,
        },

        // Install magi binaries to ~/.agents/skills/magi/bin/magi and ~/.local/bin/magi.
        Some(Command::Install) => magi::install::run().await,

        // --- Persistent configuration ---
        Some(Command::Config { command }) => match command {
            // Read a configuration key from the magi state directory (~/.magi).
            ConfigCommand::Get { key } => magi::config::get(key).await,
            // Write a configuration key-value pair to the magi state directory.
            ConfigCommand::Set { key, value } => magi::config::set(key, value).await,
        },
    }
}
