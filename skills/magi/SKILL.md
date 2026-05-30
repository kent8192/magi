---
name: magi
description: Redis-backed cross-agent messaging for Codex and other CLI agents.
---

Use `~/.agents/skills/magi/bin/magi` for all messaging operations.
Do not read or edit `~/.magi` directly.

Recommended default action (non-interactive):

```bash
~/.agents/skills/magi/bin/magi inbox
```

Running `magi` with no arguments starts an interactive REPL, which is not
suitable for automated agents; always pass an explicit subcommand.

Common actions:

```bash
~/.agents/skills/magi/bin/magi send <agent> <message>
~/.agents/skills/magi/bin/magi history
~/.agents/skills/magi/bin/magi team members
~/.agents/skills/magi/bin/magi watch --format line
~/.agents/skills/magi/bin/magi config get identity.active_team
```

First-time setup:

```bash
~/.agents/skills/magi/bin/magi redis start
~/.agents/skills/magi/bin/magi team create <team>
~/.agents/skills/magi/bin/magi invite create --team <team>
~/.agents/skills/magi/bin/magi join --invite <token>
```
