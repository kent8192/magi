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
- Node.js ≥ 22.18 (runs the TypeScript bridge via native type-stripping) and npm.

## Install (local dev marketplace)

```bash
/plugin marketplace add /absolute/path/to/magi/integrations/magi-agent-plugin
/plugin install magi-agent@magi-dev
# restart Claude Code
```

## Use

```bash
/magi-system setup     # one-time: npm install the Claude Agent SDK
/magi-system start     # start the daemon (auto-replies to messages addressed to you)
/magi-system status
/magi-system logs
/magi-system stop
```

Or call the controller directly: `integrations/magi-agent-plugin/bin/magi-agentd <subcommand>`.

## Layout

```
magi-agent-plugin/
├── .claude-plugin/
│   ├── plugin.json              # plugin manifest
│   └── marketplace.json         # local dev marketplace ("magi-dev")
├── bin/magi-agentd              # lifecycle controller (setup/start/stop/status/logs/run)
├── lib/
│   ├── magi_agent_bridge.ts     # the bridge (TypeScript, run via Node type-stripping)
│   ├── package.json             # @anthropic-ai/claude-agent-sdk dependency
│   └── node_modules/            # installed by `setup` (gitignored)
├── commands/magi-system.md      # /magi-system slash command
├── hooks/
│   ├── hooks.json               # SessionStart hook registration
│   └── magi-session-start.sh    # startup: report (and optionally boot) the magi system
└── skills/
    ├── magi-agent/SKILL.md      # the autonomous bridge
    └── magi-messaging/SKILL.md  # manual magi CLI usage in-session
```

## Startup hook

On every Claude Code session start the plugin runs `hooks/magi-session-start.sh`,
which detects the magi system state (Redis reachable? identity set? bridge up?)
and injects a one-line status as session context. It is **report-only by
default** — it never consumes your inbox and never boots anything unless you
opt in:

- `MAGI_AGENT_AUTOSTART_REDIS=1` — start managed Redis at session start if it is down.
- `MAGI_AGENT_AUTOSTART_BRIDGE=1` — start the `/magi-system` bridge daemon at session start.

If magi is not installed, the hook exits silently.

## Safety

Tools are **disabled by default** — the agent only converses. Loop prevention,
scope (`direct`/`team`), and a sender allowlist are built in. See
`skills/magi-agent/SKILL.md` for all configuration knobs and the security notes
before enabling tools (unattended tool use has real side effects).
