# magi

Redis-backed cross-agent messaging for CLI AI agents.

magi is a Rust CLI that stores team membership, invites, message history, and
per-agent inbox cursors in Redis. Redis Streams are the durable message log and
Pub/Sub is used as a low-latency wakeup for `watch`.

## Install

```bash
./install.sh
```

The installer builds the Rust binary and places it at:

- `~/.agents/skills/magi/bin/magi`
- `~/.local/bin/magi`

Configuration and managed Redis state are stored under `~/.magi`.

## Codex Plugin

This repository also includes a Codex plugin manifest at
`.codex-plugin/plugin.json`. The plugin exposes the same `magi` skill behavior
as the installed Codex skill metadata and points agents to the Rust CLI at
`~/.agents/skills/magi/bin/magi`.

## Quick Start

```bash
~/.local/bin/magi redis start
~/.local/bin/magi config set identity.active_agent alice
~/.local/bin/magi team create core
~/.local/bin/magi config set identity.active_team core
~/.local/bin/magi invite create --team core
```

On another agent:

```bash
~/.local/bin/magi config set redis.url <redis-url>
~/.local/bin/magi config set identity.active_agent bob
~/.local/bin/magi join --invite <token>
~/.local/bin/magi send alice "hello from bob"
```

## Commands

```bash
magi                          # interactive mode
magi redis start|status|stop
magi team create <team>
magi team list
magi team members [--team <team>]
magi invite create --team <team> [--ttl 24h]
magi invite list --team <team>
magi invite revoke <invite_id>
magi join --invite <token>
magi send <agent> <message>
magi inbox
magi history [--team <team>] [--agent <agent>]
magi watch [--format line|json]
magi ssh start|status|stop
magi config get <key>
magi config set <key> <value>
```

## Redis

`magi redis start` prefers Docker and falls back to `redis-server` when Docker
is unavailable. Redis auth is generated and written into `~/.magi/config.toml`;
passwords are not passed on the command line.

## Legacy Scripts

The old Bash/SQLite scripts are retired. They now exit with a clear retirement
notice and point callers to the Rust CLI.
