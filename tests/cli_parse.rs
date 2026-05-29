use clap::Parser;
use magi::cli::{
    Cli, Command, ConfigCommand, InviteCommand, RedisCommand, SshCommand, TeamCommand, WatchFormat,
};

#[test]
fn parses_default_interactive_mode() {
    let cli = Cli::try_parse_from(["magi"]).expect("parse");
    assert!(cli.command.is_none());
}

#[test]
fn parses_redis_start_lan_bind() {
    let cli = Cli::try_parse_from(["magi", "redis", "start", "--lan", "--bind", "0.0.0.0"])
        .expect("parse");

    let Some(Command::Redis {
        command: RedisCommand::Start { lan, bind },
    }) = cli.command
    else {
        panic!("expected redis start");
    };

    assert!(lan);
    assert_eq!(bind.as_deref(), Some("0.0.0.0"));
}

#[test]
fn parses_redis_start_defaults() {
    let cli = Cli::try_parse_from(["magi", "redis", "start"]).expect("parse");

    let Some(Command::Redis {
        command: RedisCommand::Start { lan, bind },
    }) = cli.command
    else {
        panic!("expected redis start");
    };

    assert!(!lan);
    assert_eq!(bind, None);
}

#[test]
fn parses_redis_status() {
    let cli = Cli::try_parse_from(["magi", "redis", "status"]).expect("parse");

    let Some(Command::Redis {
        command: RedisCommand::Status,
    }) = cli.command
    else {
        panic!("expected redis status");
    };
}

#[test]
fn parses_redis_stop() {
    let cli = Cli::try_parse_from(["magi", "redis", "stop"]).expect("parse");

    let Some(Command::Redis {
        command: RedisCommand::Stop,
    }) = cli.command
    else {
        panic!("expected redis stop");
    };
}

#[test]
fn parses_invite_create_with_ttl() {
    let cli = Cli::try_parse_from(["magi", "invite", "create", "--team", "core", "--ttl", "24h"])
        .expect("parse");

    let Some(Command::Invite {
        command: InviteCommand::Create { team, ttl },
    }) = cli.command
    else {
        panic!("expected invite create");
    };

    assert_eq!(team, "core");
    assert_eq!(ttl, "24h");
}

#[test]
fn parses_invite_create_default_ttl() {
    let cli = Cli::try_parse_from(["magi", "invite", "create", "--team", "core"]).expect("parse");

    let Some(Command::Invite {
        command: InviteCommand::Create { team, ttl },
    }) = cli.command
    else {
        panic!("expected invite create");
    };

    assert_eq!(team, "core");
    assert_eq!(ttl, "24h");
}

#[test]
fn parses_invite_list_requires_team() {
    let cli = Cli::try_parse_from(["magi", "invite", "list", "--team", "core"]).expect("parse");

    let Some(Command::Invite {
        command: InviteCommand::List { team },
    }) = cli.command
    else {
        panic!("expected invite list");
    };

    assert_eq!(team, "core");
}

#[test]
fn rejects_invite_list_without_team() {
    let error = Cli::try_parse_from(["magi", "invite", "list"]);
    assert!(error.is_err());
}

#[test]
fn parses_invite_revoke_invite_id() {
    let cli = Cli::try_parse_from(["magi", "invite", "revoke", "inv_123"]).expect("parse");

    let Some(Command::Invite {
        command: InviteCommand::Revoke { invite_id },
    }) = cli.command
    else {
        panic!("expected invite revoke");
    };

    assert_eq!(invite_id, "inv_123");
}

#[test]
fn parses_team_create() {
    let cli = Cli::try_parse_from(["magi", "team", "create", "core"]).expect("parse");
    let Some(Command::Team {
        command: TeamCommand::Create { name },
    }) = cli.command
    else {
        panic!("expected team create");
    };

    assert_eq!(name, "core");
}

#[test]
fn parses_team_list() {
    let cli = Cli::try_parse_from(["magi", "team", "list"]).expect("parse");
    let Some(Command::Team {
        command: TeamCommand::List,
    }) = cli.command
    else {
        panic!("expected team list");
    };
}

#[test]
fn parses_team_members_with_team_filter() {
    let cli = Cli::try_parse_from(["magi", "team", "members", "--team", "core"]).expect("parse");
    let Some(Command::Team {
        command: TeamCommand::Members { team },
    }) = cli.command
    else {
        panic!("expected team members");
    };

    assert_eq!(team.as_deref(), Some("core"));
}

#[test]
fn parses_team_members_without_team_filter() {
    let cli = Cli::try_parse_from(["magi", "team", "members"]).expect("parse");
    let Some(Command::Team {
        command: TeamCommand::Members { team },
    }) = cli.command
    else {
        panic!("expected team members");
    };

    assert_eq!(team, None);
}

#[test]
fn parses_send_message_tail() {
    let cli = Cli::try_parse_from(["magi", "send", "bob", "deploy", "is", "done"]).expect("parse");
    let Some(Command::Send { to, message }) = cli.command else {
        panic!("expected send");
    };

    assert_eq!(to, "bob");
    assert_eq!(message, vec!["deploy", "is", "done"]);
}

#[test]
fn rejects_send_without_message_word() {
    let error = Cli::try_parse_from(["magi", "send", "bob"]);
    assert!(error.is_err());
}

#[test]
fn parses_join_with_invite() {
    let cli = Cli::try_parse_from(["magi", "join", "--invite", "invite-token"]).expect("parse");
    let Some(Command::Join { invite }) = cli.command else {
        panic!("expected join");
    };

    assert_eq!(invite, "invite-token");
}

#[test]
fn rejects_join_without_invite() {
    let error = Cli::try_parse_from(["magi", "join"]);
    assert!(error.is_err());
}

#[test]
fn parses_history_filters() {
    let cli = Cli::try_parse_from(["magi", "history", "--team", "core", "--agent", "alice"])
        .expect("parse");
    let Some(Command::History { team, agent }) = cli.command else {
        panic!("expected history");
    };

    assert_eq!(team.as_deref(), Some("core"));
    assert_eq!(agent.as_deref(), Some("alice"));
}

#[test]
fn parses_history_without_filters() {
    let cli = Cli::try_parse_from(["magi", "history"]).expect("parse");
    let Some(Command::History { team, agent }) = cli.command else {
        panic!("expected history");
    };

    assert_eq!(team, None);
    assert_eq!(agent, None);
}

#[test]
fn parses_inbox() {
    let cli = Cli::try_parse_from(["magi", "inbox"]).expect("parse");
    let Some(Command::Inbox) = cli.command else {
        panic!("expected inbox");
    };
}

#[test]
fn parses_watch_default_line_format() {
    let cli = Cli::try_parse_from(["magi", "watch"]).expect("parse");
    let Some(Command::Watch { format }) = cli.command else {
        panic!("expected watch");
    };

    assert_eq!(format, WatchFormat::Line);
}

#[test]
fn parses_watch_json_format() {
    let cli = Cli::try_parse_from(["magi", "watch", "--format", "json"]).expect("parse");
    let Some(Command::Watch { format }) = cli.command else {
        panic!("expected watch");
    };

    assert_eq!(format, WatchFormat::Json);
}

#[test]
fn rejects_invalid_watch_format() {
    let error = Cli::try_parse_from(["magi", "watch", "--format", "xml"]);
    assert!(error.is_err());
}

#[test]
fn parses_config_get() {
    let cli = Cli::try_parse_from(["magi", "config", "get", "redis.port"]).expect("parse");
    let Some(Command::Config {
        command: ConfigCommand::Get { key },
    }) = cli.command
    else {
        panic!("expected config get");
    };

    assert_eq!(key, "redis.port");
}

#[test]
fn parses_config_set() {
    let cli = Cli::try_parse_from(["magi", "config", "set", "redis.port", "6380"]).expect("parse");
    let Some(Command::Config {
        command: ConfigCommand::Set { key, value },
    }) = cli.command
    else {
        panic!("expected config set");
    };

    assert_eq!(key, "redis.port");
    assert_eq!(value, "6380");
}

#[test]
fn parses_install_command() {
    let cli = Cli::try_parse_from(["magi", "install"]).expect("parse");
    let Some(Command::Install) = cli.command else {
        panic!("expected install");
    };
}

#[test]
fn parses_ssh_start() {
    let cli = Cli::try_parse_from(["magi", "ssh", "start"]).expect("parse");
    let Some(Command::Ssh {
        command: SshCommand::Start,
    }) = cli.command
    else {
        panic!("expected ssh start");
    };
}

#[test]
fn parses_ssh_status() {
    let cli = Cli::try_parse_from(["magi", "ssh", "status"]).expect("parse");
    let Some(Command::Ssh {
        command: SshCommand::Status,
    }) = cli.command
    else {
        panic!("expected ssh status");
    };
}

#[test]
fn parses_ssh_stop() {
    let cli = Cli::try_parse_from(["magi", "ssh", "stop"]).expect("parse");
    let Some(Command::Ssh {
        command: SshCommand::Stop,
    }) = cli.command
    else {
        panic!("expected ssh stop");
    };
}
