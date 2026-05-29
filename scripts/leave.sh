#!/usr/bin/env bash
set -euo pipefail

script_name="$(basename "$0")"
echo "magi legacy script ${script_name} is retired." >&2
echo "Use ~/.local/bin/magi or ~/.agents/skills/magi/bin/magi instead." >&2
exit 2
