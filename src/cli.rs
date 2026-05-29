use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "magi", version, about = "Redis-backed agent messaging")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Redis {
        #[command(subcommand)]
        command: RedisCommand,
    },
    Team {
        #[command(subcommand)]
        command: TeamCommand,
    },
    Invite {
        #[command(subcommand)]
        command: InviteCommand,
    },
    Join {
        #[arg(long)]
        invite: String,
    },
    Send {
        to: String,
        #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
        message: Vec<String>,
    },
    Inbox,
    History {
        #[arg(long)]
        team: Option<String>,
        #[arg(long)]
        agent: Option<String>,
    },
    Watch {
        #[arg(long, value_enum, default_value_t = WatchFormat::Line)]
        format: WatchFormat,
    },
    Ssh {
        #[command(subcommand)]
        command: SshCommand,
    },
    Install,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum RedisCommand {
    Start {
        #[arg(long)]
        lan: bool,
        #[arg(long)]
        bind: Option<String>,
    },
    Status,
    Stop,
}

#[derive(Debug, Subcommand)]
pub enum SshCommand {
    Start,
    Status,
    Stop,
}

#[derive(Debug, Subcommand)]
pub enum TeamCommand {
    Create {
        name: String,
    },
    List,
    Members {
        #[arg(long)]
        team: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum InviteCommand {
    Create {
        #[arg(long)]
        team: String,
        #[arg(long, default_value = "24h")]
        ttl: String,
    },
    List {
        #[arg(long)]
        team: String,
    },
    Revoke {
        invite_id: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Get { key: String },
    Set { key: String, value: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum WatchFormat {
    Line,
    Json,
}
