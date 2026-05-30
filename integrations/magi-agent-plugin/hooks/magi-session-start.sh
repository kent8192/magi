#!/usr/bin/env bash
#
# SessionStart hook for the magi-agent plugin.
#
# On Claude Code startup this:
#   1. Detects the magi system state (Redis reachable? identity set? bridge up?).
#   2. Optionally boots the system (opt-in via environment variables).
#   3. Injects a concise status line as SessionStart `additionalContext` so the
#      session knows whether magi messaging is available.
#
# Safe by default: it only REPORTS state. It never consumes the inbox (reading
# the inbox would advance the unread cursor), and it never starts Redis or the
# bridge unless explicitly opted in:
#   MAGI_AGENT_AUTOSTART_REDIS=1   start managed Redis if it is down
#   MAGI_AGENT_AUTOSTART_BRIDGE=1  start the /magi-system bridge daemon
#
# If magi is not installed, the hook exits silently so it never disturbs a
# session in a project that does not use magi.
set -uo pipefail

PLUGIN_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Resolve the magi binary: explicit override, then PATH, then install locations.
MAGI="${MAGI_BIN:-}"
[ -n "$MAGI" ] || MAGI="$(command -v magi 2>/dev/null || true)"
if [ -z "$MAGI" ]; then
  for c in "$HOME/.agents/skills/magi/bin/magi" "$HOME/.local/bin/magi"; do
    [ -x "$c" ] && MAGI="$c" && break
  done
fi
# Not installed -> do nothing, do not break session startup.
{ [ -n "$MAGI" ] && [ -x "$MAGI" ]; } || exit 0

truthy() {
  case "$(printf '%s' "${1:-}" | tr '[:upper:]' '[:lower:]')" in
    1 | true | yes | on) return 0 ;;
    *) return 1 ;;
  esac
}

# Strip characters that would break the hand-built JSON below.
sanitize() { printf '%s' "${1:-}" | tr -d '"\\\n\r' ; }

redis_reachable() { "$MAGI" redis status >/dev/null 2>&1; }

# --- Optional Redis autostart -------------------------------------------------
if ! redis_reachable && truthy "${MAGI_AGENT_AUTOSTART_REDIS:-}"; then
  "$MAGI" redis start >/dev/null 2>&1 || true
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    redis_reachable && break
    sleep 0.5
  done
fi

if redis_reachable; then
  redis_state="reachable"
else
  redis_state="DOWN"
fi

agent="$(sanitize "$("$MAGI" config get identity.active_agent 2>/dev/null)")"
team="$(sanitize "$("$MAGI" config get identity.active_team 2>/dev/null)")"

# --- Bridge daemon state (read the daemon's pid file directly) ----------------
STATE_DIR="${MAGI_AGENT_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/magi-agent}"
PID_FILE="$STATE_DIR/agentd.pid"
bridge_running() {
  [ -f "$PID_FILE" ] || return 1
  local pid
  pid="$(cat "$PID_FILE" 2>/dev/null || true)"
  [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null
}

# --- Optional bridge autostart ------------------------------------------------
if ! bridge_running && truthy "${MAGI_AGENT_AUTOSTART_BRIDGE:-}"; then
  "$PLUGIN_ROOT/bin/magi-agentd" start >/dev/null 2>&1 || true
fi

if bridge_running; then
  bridge_state="running"
else
  bridge_state="stopped"
fi

ctx="magi messaging available. Redis: ${redis_state}; agent: ${agent:-unset}; team: ${team:-unset}; auto-reply bridge: ${bridge_state}. Use the magi CLI for messaging (send/inbox/history/team) and /magi-system to control the bridge; do not read or edit ~/.magi directly."

# Emit SessionStart additionalContext as JSON.
printf '{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"%s"}}\n' "$ctx"
exit 0
