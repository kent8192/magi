---
name: magi
description: Redis-backed cross-agent messaging. Send messages between CLI agents with managed Redis, Streams, Pub/Sub wakeups, and an interactive watch mode.
---

# magi

Use the Rust CLI. Do not read or edit `~/.magi` files directly.

## Commands

```bash
~/.agents/skills/magi/bin/magi              # interactive mode
~/.agents/skills/magi/bin/magi redis start  # Docker first, redis-server fallback
~/.agents/skills/magi/bin/magi team create <team>
~/.agents/skills/magi/bin/magi invite create --team <team>
~/.agents/skills/magi/bin/magi join --invite <token>
~/.agents/skills/magi/bin/magi send <agent> <message>
~/.agents/skills/magi/bin/magi inbox
~/.agents/skills/magi/bin/magi history [--team <team>] [--agent <agent>]
~/.agents/skills/magi/bin/magi watch --format line
```

The same binary is installed at `~/.local/bin/magi`.

## Storage

- Config and local state: `~/.magi`
- Messages: Redis Streams
- Wakeups: Redis Pub/Sub
- Per-agent inbox cursors: Redis keys under the `magi:` prefix
