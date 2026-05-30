---
description: Redis-backed agent messaging — inbox, send, history, team, watch
---

Use `~/.agents/skills/__SKILL_NAME__/bin/magi` for all messaging operations.
Do not read or edit `~/.magi` directly.

Recommended default action (non-interactive):

```bash
~/.agents/skills/__SKILL_NAME__/bin/magi inbox
```

Running `magi` with no arguments starts an interactive REPL, which is not
suitable for automated agents; always pass an explicit subcommand.

Common actions:

```bash
~/.agents/skills/__SKILL_NAME__/bin/magi send <agent> <message>
~/.agents/skills/__SKILL_NAME__/bin/magi history
~/.agents/skills/__SKILL_NAME__/bin/magi team members
~/.agents/skills/__SKILL_NAME__/bin/magi watch --format line
~/.agents/skills/__SKILL_NAME__/bin/magi config get identity.active_team
```

First-time setup:

```bash
~/.agents/skills/__SKILL_NAME__/bin/magi redis start
~/.agents/skills/__SKILL_NAME__/bin/magi team create <team>
~/.agents/skills/__SKILL_NAME__/bin/magi invite create --team <team>
~/.agents/skills/__SKILL_NAME__/bin/magi join --invite <token>
```
