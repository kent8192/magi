---
description: Control the magi-agent bridge (setup/start/stop/status/logs) that turns incoming magi messages into a live Claude session
argument-hint: "[setup|start|stop|restart|status|logs|run]"
allowed-tools: Bash(${CLAUDE_PLUGIN_ROOT}/bin/magi-agentd:*)
---

Run the magi-agent lifecycle controller with the requested subcommand and report the result concisely.

Subcommand requested by the user: `$ARGUMENTS`

Steps:

1. If `$ARGUMENTS` is empty, default to `status`.
2. Execute the controller:

   ```bash
   "${CLAUDE_PLUGIN_ROOT}/bin/magi-agentd" $ARGUMENTS
   ```

3. Summarize the outcome for the user:
   - For `setup`: confirm the virtualenv and Claude Agent SDK installed.
   - For `start`: confirm it is running (pid) and where logs are; remind them the
     agent now auto-replies to messages addressed to the active magi agent.
   - For `stop`: confirm it stopped.
   - For `status`: report running/stopped, the active `self`/`team` identity, and
     surface anything notable in the recent log lines.
   - For `logs`: this follows the log; tell the user to press Ctrl-C to stop.

Notes:
- First-time use requires `setup` once (creates an isolated venv; no global Python is modified).
- The bridge requires a reachable magi Redis (`magi redis status`) and a set
  `identity.active_agent`. If `start` fails, run `magi redis start` and verify
  `magi config get identity.active_agent`, then retry.
- Behavior is tuned with environment variables documented in the `magi-agent`
  skill (scope, auto-reply, allowed tools, system prompt, model).
