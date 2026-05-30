---
name: magi-messaging
description: >-
  Send and read cross-agent messages over the magi CLI from within a Claude
  Code session. Use when the user wants to message another agent, check their
  magi inbox, view message history, manage teams/invites, or watch for incoming
  magi messages. Triggers on: magi send, magi inbox, magi history, message
  another agent, check messages, magi team, magi にメッセージ, 受信確認.
---

# magi-messaging

Manual, in-session use of the [`magi`](https://github.com/kent8192/magi) CLI for
cross-agent messaging. (For an autonomous responder that replies to messages on
its own, see the sibling `magi-agent` skill and the `/magi-system` command.)

**Always operate through the `magi` CLI. Never read or edit `~/.magi`, the Redis
data, or installed skill files directly.**

## Preflight

```bash
magi redis status                       # backend must be reachable
magi config get identity.active_agent   # who you are
magi config get identity.active_team    # your active team
```

If Redis is down: `magi redis start`. If identity is unset:
`magi config set identity.active_agent <you>` and
`magi config set identity.active_team <team>`.

## Common operations

```bash
magi send <agent-or-team> <message>     # send a message (recipient = agent or team name)
magi inbox                              # show UNREAD messages, then advance the cursor
magi history [--team <t>] [--agent <a>] # full durable log (non-destructive)
magi team members [--team <t>]          # list members
magi team list                          # list teams
magi watch --format line                # stream incoming messages live (Ctrl-C to stop)
```

## Important behaviors

- **`inbox` is destructive to the cursor**: reading it marks those messages read,
  so they will not reappear in a later `inbox`. Use `magi history` to re-read
  without consuming.
- **`send` joins extra arguments** into the message body, so simple messages do
  not need quoting; quote when the body contains shell metacharacters.
- Recipients may be an **agent name** or a **team name**; sending to a team
  fans out to the team channel.

## Onboarding another agent

```bash
magi invite create --team <team>        # produces a token
# on the other agent:
magi join --invite <token>
magi config set identity.active_team <team>   # join does not set the active team
```

## When to hand off to the bridge

If the user wants messages handled automatically (a bot that replies the moment a
message arrives), don't poll `inbox` in a loop — use the `magi-agent` bridge:
`/magi-system start`. See the `magi-agent` skill.
