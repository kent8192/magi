#!/usr/bin/env bash
set -euo pipefail

# magi — Redis-backed agent messaging installer
#
# Installs:
#   ~/.agents/skills/magi/bin/magi
#   ~/.local/bin/magi
#
# Configuration and Redis state live under:
#   ~/.magi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SKILL_DIR="$HOME/.agents/skills/magi"
SKILL_BIN="$SKILL_DIR/bin/magi"
LOCAL_CLI="$HOME/.local/bin/magi"

if ! command -v cargo >/dev/null 2>&1; then
  echo "Error: cargo is required to build magi." >&2
  exit 1
fi

echo "magi — Redis-backed agent messaging"
echo "building release binary..."
CARGO_TARGET_DIR="$SCRIPT_DIR/target" cargo build --release --manifest-path "$SCRIPT_DIR/Cargo.toml"

mkdir -p "$SKILL_DIR/bin" "$SKILL_DIR/templates" "$SKILL_DIR/agents" "$HOME/.local/bin" "$HOME/.magi"
install -m 0755 "$SCRIPT_DIR/target/release/magi" "$SKILL_BIN"
install -m 0755 "$SCRIPT_DIR/target/release/magi" "$LOCAL_CLI"

sed "s/__SKILL_NAME__/magi/g" "$SCRIPT_DIR/templates/cmd.codex.md" > "$SKILL_DIR/SKILL.md"
for tmpl in "$SCRIPT_DIR/templates/"cmd.*.md; do
  sed "s/__SKILL_NAME__/magi/g" "$tmpl" > "$SKILL_DIR/templates/$(basename "$tmpl")"
done
cp "$SCRIPT_DIR/openai.yaml" "$SKILL_DIR/agents/openai.yaml" 2>/dev/null || true

if [ -d "$HOME/.claude" ]; then
  mkdir -p "$HOME/.claude/commands"
  sed "s/__SKILL_NAME__/magi/g" "$SCRIPT_DIR/templates/cmd.claude-code.md" > "$HOME/.claude/commands/magi.md"
fi

"$LOCAL_CLI" install

cat <<MSG

Installed magi:
  $SKILL_BIN
  $LOCAL_CLI

Configuration:
  $HOME/.magi

Next:
  1. Run: ~/.local/bin/magi redis start
  2. Run: ~/.local/bin/magi team create <team>
  3. Run: ~/.local/bin/magi invite create --team <team>

MSG
