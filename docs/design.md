# magi Design

## Architecture

- CLI: Rust, `clap`, Tokio
- State directory: `~/.magi`
- Install locations: `~/.agents/skills/magi/bin/magi`, `~/.local/bin/magi`
- Codex plugin surface: `.codex-plugin/plugin.json` with `skills/magi/`
  mirroring the installed Codex skill instructions.
- Redis lifecycle: Docker first, `redis-server` fallback
- Durable messaging: Redis Streams
- Wakeups: Redis Pub/Sub
- Inbox tracking: one Redis cursor per `(team, agent)`

## Redis Key Model

All keys use the `magi:` prefix. Team and agent segments are percent-encoded so
IDs containing separators cannot collide with normal key shapes.

Important keys:

- `magi:teams`
- `magi:team:<team>`
- `magi:team:<team>:agents`
- `magi:agent:<team>:<agent>`
- `magi:stream:<team>`
- `magi:cursor:<team>:<agent>`
- `magi:pubsub:<team>`
- `magi:invite:<invite_id>`
- `magi:invite_token:<token_hash>`

## Messages

Messages are appended to `magi:stream:<team>` with fields:

- `from`
- `to`
- `body`
- `created_at`

`magi inbox` reads from the stored cursor, prints messages addressed to the
active agent, and advances the cursor to the last scanned stream entry.

`magi watch` subscribes to `magi:pubsub:<team>` and also polls periodically, so
missed Pub/Sub wakeups do not lose durable Stream messages.

## Invites

Invite tokens are generated randomly. Redis stores only a SHA-256 token hash and
a lookup key with TTL. Joining is guarded by a Lua script so revoked, expired,
or exhausted invites cannot race through concurrent joins.

## SSH

`magi ssh start` creates an SSH local port-forward from the configured
`ssh.local_port` to `ssh.remote_host:ssh.remote_port` via `ssh.host`, and stores
the process id under `~/.magi/run`.

## Retired Bash Scripts

The former Bash/SQLite scripts remain only as compatibility stubs. Each exits
with code `2` and directs callers to the Rust CLI.
