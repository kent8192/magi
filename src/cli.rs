//! Command-line interface definition for the `magi` CLI tool.
//!
//! This module uses `clap` to declare every subcommand, flag, and argument
//! that `magi` accepts.  It is purely declarative — no business logic lives
//! here.  After `clap` parses `argv`, the resulting `Cli` value is handed
//! off to `main` (or a dispatcher in `lib.rs`) which routes each `Command`
//! variant to the appropriate handler module (Redis lifecycle, messaging,
//! team management, SSH helpers, installer, etc.).
//!
//! # Top-level structure
//!
//! ```text
//! magi [SUBCOMMAND]
//!   redis   {start|status|stop}
//!   team    {create|list|members}
//!   invite  {create|list|revoke}
//!   join    --invite <TOKEN>
//!   send    <TO> <MESSAGE>...
//!   inbox
//!   history [--team <T>] [--agent <A>]
//!   watch   [--format line|json]
//!   ssh     {start|status|stop}
//!   install
//!   config  {get|set}
//! ```
//!
//! When invoked with no subcommand, `magi` falls back to an interactive REPL
//! (handled by the caller when `Cli::command` is `None`).

use clap::{Parser, Subcommand, ValueEnum};

/// Root CLI struct parsed from `argv` by `clap`.
///
/// `command` is `None` when the user runs `magi` with no subcommand, which
/// signals the caller to enter interactive REPL mode.
#[derive(Debug, Parser)]
#[command(name = "magi", version, about = "Redis-backed agent messaging")]
pub struct Cli {
    /// The subcommand to execute, or `None` to enter interactive REPL mode.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// All top-level subcommands supported by `magi`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the embedded (managed) Redis server lifecycle.
    ///
    /// The managed Redis process is owned by `magi` and stores its PID and
    /// socket information under `~/.magi/redis/`.  Use these subcommands to
    /// start, query, or stop it without needing a separately installed Redis.
    Redis {
        #[command(subcommand)]
        command: RedisCommand,
    },

    /// Manage agent teams (create teams, list them, view members).
    Team {
        #[command(subcommand)]
        command: TeamCommand,
    },

    /// Manage invite tokens used for invite-based onboarding.
    ///
    /// Invite tokens are stored in Redis and carry an expiry (`--ttl`).
    /// A remote agent redeems a token via the `join` subcommand.
    Invite {
        #[command(subcommand)]
        command: InviteCommand,
    },

    /// Redeem an invite token to join a team.
    ///
    /// This is the counterpart to `invite create`.  The `--invite` argument
    /// must be a valid, non-expired token produced by `magi invite create`.
    Join {
        /// The invite token to redeem (produced by `magi invite create`).
        #[arg(long)]
        invite: String,
    },

    /// Send a message to another agent or team via Redis Streams.
    ///
    /// `to` identifies the recipient (agent name or team name).  All
    /// remaining positional arguments are joined into the message body,
    /// allowing callers to omit shell quoting for simple messages.
    Send {
        /// Recipient agent or team name.
        to: String,
        /// Message words; multiple tokens are joined with spaces before delivery.
        #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
        message: Vec<String>,
    },

    /// Display unread messages in the current agent's inbox.
    Inbox,

    /// Display message history, optionally filtered by team or agent.
    ///
    /// Without filters the full stream history visible to this agent is shown.
    History {
        /// Restrict output to messages belonging to this team.
        #[arg(long)]
        team: Option<String>,
        /// Restrict output to messages sent by or to this agent.
        #[arg(long)]
        agent: Option<String>,
    },

    /// Subscribe to the Redis Pub/Sub channel and stream incoming messages.
    ///
    /// Runs until interrupted (Ctrl-C).  Use `--format json` to emit
    /// newline-delimited JSON suitable for machine consumption; the default
    /// `line` format is human-readable.
    Watch {
        /// Output format: `line` (human-readable) or `json` (NDJSON).
        #[arg(long, value_enum, default_value_t = WatchFormat::Line)]
        format: WatchFormat,
    },

    /// Manage the SSH helper process used for secure remote connections.
    ///
    /// The SSH helper facilitates agent-to-agent communication across hosts.
    /// Its lifecycle (PID file, port) is tracked under `~/.magi/ssh/`.
    Ssh {
        #[command(subcommand)]
        command: SshCommand,
    },

    /// Install `magi` binaries and set up the `~/.magi` state directory.
    ///
    /// Places the primary binary at `~/.agents/skills/magi/bin/magi` and
    /// creates a symlink at `~/.local/bin/magi` so the command is on `PATH`.
    Install,

    /// Read or write persistent `magi` configuration values.
    ///
    /// Configuration is stored as key-value pairs under `~/.magi/config`.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

/// Subcommands for managing the embedded Redis server.
#[derive(Debug, Subcommand)]
pub enum RedisCommand {
    /// Start the managed Redis server.
    ///
    /// By default Redis binds only to `127.0.0.1`.  Pass `--lan` to also
    /// bind on the LAN interface, or `--bind` to specify an address explicitly.
    Start {
        /// Bind on the LAN interface in addition to loopback.
        #[arg(long)]
        lan: bool,
        /// Explicit bind address (overrides `--lan`).
        #[arg(long)]
        bind: Option<String>,
    },
    /// Report whether the managed Redis server is running and print its address.
    Status,
    /// Stop the managed Redis server gracefully.
    Stop,
}

/// Subcommands for managing the SSH helper process.
#[derive(Debug, Subcommand)]
pub enum SshCommand {
    /// Start the SSH helper process.
    Start,
    /// Report the SSH helper's running status and listening port.
    Status,
    /// Stop the SSH helper process.
    Stop,
}

/// Subcommands for team management.
#[derive(Debug, Subcommand)]
pub enum TeamCommand {
    /// Create a new team with the given name.
    Create {
        /// Unique name for the new team.
        name: String,
    },
    /// List all teams the current agent belongs to.
    List,
    /// List the members of a team.
    ///
    /// If `--team` is omitted, the default team for this agent is used.
    Members {
        /// Name of the team whose members to list.
        #[arg(long)]
        team: Option<String>,
    },
}

/// Subcommands for invite-token management.
#[derive(Debug, Subcommand)]
pub enum InviteCommand {
    /// Create a new invite token for a team.
    ///
    /// The token is stored in Redis with the specified TTL and printed to
    /// stdout so it can be shared with the agent being invited.
    Create {
        /// The team the invitee will join upon redemption.
        #[arg(long)]
        team: String,
        /// Time-to-live for the invite token (e.g. `"24h"`, `"7d"`).
        #[arg(long, default_value = "24h")]
        ttl: String,
    },
    /// List active (non-expired) invite tokens for a team.
    List {
        /// Team whose invite tokens to list.
        #[arg(long)]
        team: String,
    },
    /// Revoke an invite token immediately, regardless of its remaining TTL.
    Revoke {
        /// The ID of the invite token to revoke.
        invite_id: String,
    },
}

/// Subcommands for reading or writing persistent configuration.
#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Read a configuration value by key.
    Get { key: String },
    /// Write a configuration value.
    Set { key: String, value: String },
}

/// Output format used by the `watch` subcommand.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum WatchFormat {
    /// Human-readable one-message-per-line format (default).
    Line,
    /// Newline-delimited JSON (NDJSON) for machine consumption.
    Json,
}
