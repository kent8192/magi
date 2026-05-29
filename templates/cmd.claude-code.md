---
description: Redis-backed agent messaging — inbox, send, history, team, watch
---

Use `~/.agents/skills/__SKILL_NAME__/bin/magi` for all messaging operations.
Do not read or edit `~/.magi` directly.

Default action with no arguments:

```bash
~/.agents/skills/__SKILL_NAME__/bin/magi inbox
```

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
