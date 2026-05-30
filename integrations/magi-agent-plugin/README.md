# magi-agent (Claude Code plugin)

Event-driven bridge that turns **incoming magi messages into a live Claude
session**. The moment a teammate sends you a magi message, its text becomes a new
user turn in a persistent [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk)
conversation, and the assistant's reply is delivered back through `magi send`.

This is the "Plan A" architecture: one long-lived process holding persistent SDK
sessions (one per peer), fed by magi's Redis Pub/Sub stream (`magi watch`), so
delivery is instant rather than polled.

## Requirements

- [`magi`](https://github.com/kent8192/magi) installed and a reachable Redis
  (`magi redis status`), with `identity.active_agent` set.
- The `claude` CLI on `PATH` (the SDK drives it; uses your existing Claude auth).
- Python ≥ 3.10 available to build an isolated virtualenv (no global Python is touched).

## Install (local dev marketplace)

```bash
/plugin marketplace add /absolute/path/to/magi/integrations/magi-agent-plugin
/plugin install magi-agent@magi-dev
# restart Claude Code
```

## Use

```bash
/magi-agent setup     # one-time: build venv + install claude-agent-sdk
/magi-agent start     # start the daemon (auto-replies to messages addressed to you)
/magi-agent status
/magi-agent logs
/magi-agent stop
```

Or call the controller directly: `integrations/magi-agent-plugin/bin/magi-agentd <subcommand>`.

## Layout

```
magi-agent-plugin/
├── .claude-plugin/
│   ├── plugin.json          # plugin manifest
│   └── marketplace.json     # local dev marketplace ("magi-dev")
├── bin/magi-agentd          # lifecycle controller (setup/start/stop/status/logs/run)
├── lib/
│   ├── magi_agent_bridge.py # the asyncio bridge (claude-agent-sdk)
│   └── requirements.txt
├── commands/magi-agent.md   # /magi-agent slash command
└── skills/magi-agent/SKILL.md
```

## Safety

Tools are **disabled by default** — the agent only converses. Loop prevention,
scope (`direct`/`team`), and a sender allowlist are built in. See
`skills/magi-agent/SKILL.md` for all configuration knobs and the security notes
before enabling tools (unattended tool use has real side effects).
