use clap::Parser;
use magi::cli::{
    Cli, Command, ConfigCommand, InviteCommand, RedisCommand, SshCommand, TeamCommand,
};
use magi::error::Result;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    match cli.command {
        None => magi::repl::run().await,
        Some(Command::Redis { command }) => match command {
            RedisCommand::Start { lan, bind } => magi::redis_manager::start(lan, bind).await,
            RedisCommand::Status => magi::redis_manager::status().await,
            RedisCommand::Stop => magi::redis_manager::stop().await,
        },
        Some(Command::Team { command }) => match command {
            TeamCommand::Create { name } => magi::team::create(name).await,
            TeamCommand::List => magi::team::list().await,
            TeamCommand::Members { team } => magi::team::members(team).await,
        },
        Some(Command::Invite { command }) => match command {
            InviteCommand::Create { team, ttl } => magi::invite::create(team, ttl).await,
            InviteCommand::List { team } => magi::invite::list(team).await,
            InviteCommand::Revoke { invite_id } => magi::invite::revoke(invite_id).await,
        },
        Some(Command::Join { invite }) => magi::invite::join(invite).await,
        Some(Command::Send { to, message }) => magi::messaging::send(to, message).await,
        Some(Command::Inbox) => magi::messaging::inbox().await,
        Some(Command::History { team, agent }) => magi::messaging::history(team, agent).await,
        Some(Command::Watch { format }) => magi::watch::run(format).await,
        Some(Command::Ssh { command }) => match command {
            SshCommand::Start => magi::ssh::start().await,
            SshCommand::Status => magi::ssh::status().await,
            SshCommand::Stop => magi::ssh::stop().await,
        },
        Some(Command::Install) => magi::install::run().await,
        Some(Command::Config { command }) => match command {
            ConfigCommand::Get { key } => magi::config::get(key).await,
            ConfigCommand::Set { key, value } => magi::config::set(key, value).await,
        },
    }
}
