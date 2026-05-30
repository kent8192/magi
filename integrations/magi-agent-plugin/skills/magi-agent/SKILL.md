---
name: magi-agent
description: >-
  Event-driven bridge that turns incoming magi messages into a persistent
  Claude Agent SDK session and auto-replies via the magi CLI. Use when the
  user wants magi messages to drive Claude automatically, asks to run/configure
  the "magi agent", "magi bot", or an autonomous responder over magi, or wants
  the agent to act the moment a message arrives. Triggers on: magi agent, magi
  bot, auto-reply magi, magi 常駐エージェント, 着信で自動応答.
---

# magi-agent

A long-lived process ("Plan A") that subscribes to the magi message stream and,
the instant a message arrives, feeds its body into a **persistent Claude Agent
SDK conversation** as a new user turn. The assistant's reply is sent back to the
originating agent through the magi CLI. No polling — magi's Redis Pub/Sub wakeup
makes delivery instant.

```
magi send ──▶ Redis Pub/Sub ──▶ `magi watch --format json`
                                        │ (NDJSON line per message)
                                        ▼
                              magi_agent_bridge.py
                                        │  persistent ClaudeSDKClient (per peer)
                                        ▼
                              assistant reply text
                                        │
                                        ▼
                               `magi send <peer> -- <reply>`
```

## Components

- `bin/magi-agentd` — lifecycle controller (`setup|start|stop|restart|status|logs|run`).
- `lib/magi_agent_bridge.py` — the asyncio bridge built on `claude-agent-sdk`.
- `commands/magi-agent.md` — the `/magi-agent` slash command wrapping the controller.

Runtime state (virtualenv, pid, log) lives under
`${XDG_STATE_HOME:-~/.local/state}/magi-agent/`, never inside the plugin.

## Quick start

```bash
magi redis start                                   # backend must be reachable
magi config set identity.active_agent <you>        # the agent the bridge speaks as
magi config set identity.active_team <team>

/magi-agent setup     # one-time: builds an isolated venv + installs the SDK
/magi-agent start     # launches the daemon
/magi-agent status    # running? identity? recent log
/magi-agent stop
```

From another agent, send a message to `<you>` and the bridge replies automatically.

## How conversations are kept

Each remote peer (the `from` field of a message) gets its **own** persistent
`ClaudeSDKClient`, so every counterpart has an independent, continuous
conversation. Clients are created lazily and capped with LRU eviction
(`MAGI_AGENT_MAX_PEERS`). Incoming messages are processed **serially** so each
turn's response boundary is unambiguous.

## Guardrails (important)

- **Loop prevention:** messages where `from == self` are always ignored — without
  this, the agent would answer its own replies forever.
- **Scope:** only messages addressed to you are handled (`MAGI_AGENT_SCOPE=direct`,
  default). Set `team` to also handle messages addressed to the active team.
- **Sender allowlist:** `MAGI_AGENT_ALLOW_FROM=alice,bob` restricts who can drive
  the agent.
- **Tools off by default:** `allowed_tools=[]` — the agent only converses and never
  runs commands unattended. Enable explicitly (see below) only when you trust the
  senders and understand the risk.

## Configuration (environment variables)

| Variable | Default | Meaning |
|---|---|---|
| `MAGI_BIN` | `magi` on PATH | Path to the magi binary |
| `MAGI_AGENT_SELF` | `identity.active_agent` | Agent name the bridge speaks as |
| `MAGI_AGENT_TEAM` | `identity.active_team` | Active team (for `team` scope) |
| `MAGI_AGENT_SCOPE` | `direct` | `direct` (to me) or `team` (to me or my team) |
| `MAGI_AGENT_AUTO_REPLY` | `1` | Send the reply back (`0` = process only, no send) |
| `MAGI_AGENT_ALLOW_FROM` | (all) | Comma list of senders allowed to drive the agent |
| `MAGI_AGENT_SYSTEM_PROMPT` | built-in | System prompt for the responder |
| `MAGI_AGENT_ALLOWED_TOOLS` | (none) | Comma list of tools to enable (opt-in) |
| `MAGI_AGENT_PERMISSION_MODE` | `default` | SDK permission mode (`acceptEdits`, `bypassPermissions`, …) — only relevant when tools are enabled |
| `MAGI_AGENT_MODEL` | SDK default | Model id override |
| `MAGI_AGENT_MAX_REPLY_CHARS` | `4000` | Truncate outgoing replies to this length |
| `MAGI_AGENT_MAX_PEERS` | `8` | Max concurrent persistent peer sessions |
| `MAGI_AGENT_CWD` | `~` | Working directory for the Claude session |
| `MAGI_AGENT_SETTING_SOURCES` | (lean, none) | Comma list of `user,project,local` settings to load; default is none so the bot ignores your global CLAUDE.md |
| `MAGI_AGENT_PYTHON` | autodetect | Interpreter used to build the venv (needs ≥3.10) |
| `MAGI_AGENT_STATE_DIR` | `~/.local/state/magi-agent` | Where venv/pid/log live |

Enabling tools is unattended automation with real side effects. If you opt in,
prefer a narrow `MAGI_AGENT_ALLOWED_TOOLS` set and a non-interactive
`MAGI_AGENT_PERMISSION_MODE` (the daemon cannot answer interactive prompts).

## Troubleshooting

- `start` fails immediately → run `/magi-agent status` and read the log tail; usual
  causes are `identity.active_agent` unset or `magi redis status` unreachable.
- No replies → check scope/allowlist, and confirm the sender isn't your own agent
  name (self-messages are ignored by design).
- Verify the SDK loop in the foreground with `/magi-agent run` (Ctrl-C to stop).
