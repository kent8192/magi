# CLAUDE.md

## Purpose

This file contains project-specific instructions for the magi project. These
rules keep agent work scoped, testable, and consistent across the Rust CLI, the
Redis-backed message store, the retired Bash stubs, templates, and
documentation.

For project behavior and architecture, see `README.md`, `docs/design.md`, and
`SKILL.md`.

---

## Project Overview

`magi` provides cross-agent messaging for CLI AI agents through a Rust CLI
backed by Redis. Team membership, invites, message history, and per-agent inbox
cursors live in Redis: Redis Streams are the durable message log and Pub/Sub is
a low-latency wakeup for `watch`.

There is no long-running magi daemon. The CLI manages the Redis lifecycle
(Docker first, `redis-server` fallback) and an optional SSH tunnel for reaching
a remote Redis. Local configuration and managed Redis state live under `~/.magi`,
and the binary is installed at `~/.agents/skills/magi/bin/magi` and
`~/.local/bin/magi`.

**Repository URL**: https://github.com/kent8192/magi

---

## Tech Stack

- **Language**: Rust (edition 2021), `clap` CLI, Tokio async runtime
- **Storage**: Redis (Streams for durable messages, Pub/Sub for wakeups)
- **Redis lifecycle**: Docker container first, local `redis-server` fallback
- **Tests**: `cargo test` (unit plus Redis-gated integration) and a BATS suite
  for the retired Bash stubs under `tests/`
- **Install targets**: Claude Code slash command and Codex skill metadata
- **Templates**: `templates/cmd.claude-code.md` and `templates/cmd.codex.md`
- **Documentation**: `README.md`, `docs/design.md`, and `SKILL.md`

---

## Critical Rules

### Data and Configuration Access

**MUST use the `magi` CLI for all project data operations.**

- Use `magi team create`, `magi invite create`, `magi join`, `magi send`,
  `magi inbox`, `magi history`, and `magi team members` for team and message
  operations.
- Use `magi redis start|status|stop` and `magi ssh start|status|stop` for the
  managed Redis and SSH tunnel lifecycle.
- Use `magi config get|set` for configuration. Do not hand-edit
  `~/.magi/config.toml`, Redis data, or generated installed skill files unless
  the task is explicitly about the installer or migration behavior.
- The Bash scripts under `scripts/` are retired compatibility stubs that exit
  with code `2`. Do not reintroduce SQLite or shell-based messaging behavior in
  them.
- Redis is the backing store and is reached over the network. Preserve that
  model and the managed-lifecycle design rather than adding a separate daemon.

### Code Style

- **ALL code comments MUST be written in English**.
- Follow standard Rust style. Keep the code `cargo fmt` clean and free of
  `cargo clippy --all-targets -- -D warnings` findings.
- Prefer explicit error messages and deterministic exit codes. Return typed
  `MagiError` values rather than panicking on expected failures.
- Keep the retired stub scripts in Bash with `set -euo pipefail`.
- Limit dependencies to crates already declared in `Cargo.toml`. Do not add new
  runtime services or network dependencies beyond the existing Redis client and
  SSH tunnel.
- Mark placeholders with `TODO:` only when they represent intentional future
  work; remove obsolete TODOs when implementing the behavior.

### File Management

- Do not write build artifacts or temporary files into the project directory.
  Build into a temporary directory (for example `mktemp -d`) and clean up; the
  `target/` directory is git-ignored.
- Delete backup files (`.bak`, `.backup`, `.old`, `~`) when no longer needed.
- Do not modify user configuration files such as `~/.codex/config.toml` or the
  real `~/.magi` state directly from repository work. Provide commands or
  templates instead.
- Preserve unrelated working-tree changes, including local editor/tooling state.

### Testing

- New or changed behavior should have focused coverage: `cargo test` for the
  Rust CLI and BATS for the retired stub scripts.
- Redis-backed integration tests read `MAGI_TEST_REDIS_URL` and skip when it is
  unset; set `MAGI_REQUIRE_REDIS_TESTS=1` to require them in CI.
- Tests MUST isolate `HOME` and must not read or write the real `~/.magi` state
  or `~/.agents/skills/magi` install directory.
- Prefer meaningful assertions over output smoke checks.
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `bash -n` for any changed scripts.

### Documentation

- Update `README.md`, `docs/design.md`, `SKILL.md`, and command templates when
  behavior or user-facing commands change.
- Keep installed-command behavior and documentation synchronized between
  Claude Code and Codex templates.
- Documentation must describe technical rationale and behavior. Do not document
  user requests or AI assistant interactions.

**CLAUDE.md <-> AGENTS.md Sync Policy:**
- `CLAUDE.md` (Claude Code) and `AGENTS.md` (Codex) are deliberate mirror copies
  kept in sync.
- The two files MUST differ only on this small set of mechanical substitutions:
  - `CLAUDE.md` <-> `AGENTS.md` in titles and references
  - `CLAUDE.local.md` <-> `AGENTS.local.md`
  - `Claude Code` <-> `Codex` when describing agent-specific attribution or
    local preferences
- Any edit to one file MUST be mirrored into the other in the same change.
- After editing, run `diff CLAUDE.md AGENTS.md` and confirm only the documented
  substitutions remain.

---

## Git Workflow

- Committing and pushing to the fork (`origin`, `kent8192/magi`) is allowed
  without a separate explicit instruction. Keep changes scoped and split commits
  by specific intent.
- Pull Requests may be opened, but every Pull Request MUST target the fork
  (`kent8192/magi`) as its base repository (see Upstream Repository Protection).
- Never use destructive Git operations such as `git reset --hard`, force-push,
  or branch deletion without explicit authorization.
- The Upstream Repository Protection rules below remain in force at all times and
  are never relaxed by the allowance above.
- Use `gh` for GitHub operations when needed.

### Upstream Repository Protection

This repository is a fork. `origin` is the working fork, and `upstream` is the
source repository.

**NEVER perform operations that affect upstream repository state.**

**NEVER create Pull Requests against upstream.** All Pull Requests for this
repository MUST be created in the fork repository (`kent8192/magi`) with an
explicit repository target, for example:

```bash
gh pr create --repo kent8192/magi --base main --head <branch>
```

Do not rely on `gh pr create` defaults, because this checkout also has an
`upstream` remote and GitHub may infer the source repository as the upstream
project.

Prohibited operations include:

- `git push upstream ...`, including branch pushes, tag pushes, deletions, and
  force pushes.
- Creating, editing, closing, merging, labeling, or commenting on upstream
  Issues, Pull Requests, Discussions, Releases, or Actions runs.
- Creating Pull Requests whose base repository is upstream, including
  cross-repository PRs from `kent8192/magi` to `fujibee/agmsg`.
- Running `gh` commands against upstream, including `gh -R fujibee/agmsg ...`,
  unless the command is strictly read-only.
- Triggering, re-running, canceling, approving, or otherwise modifying upstream
  GitHub Actions workflows.
- Enabling or relying on maintainer edits from upstream maintainers when it
  would allow upstream-side changes to fork branches with sensitive workflow
  effects.

Allowed read-only operations:

- `git fetch upstream`
- `git remote -v`
- `gh repo view fujibee/agmsg`
- `gh pr view`, `gh issue view`, or `gh run view` against upstream when no
  mutation occurs

If an upstream-impacting action appears necessary, stop and ask the user for
explicit authorization that names the upstream repository and the exact
operation.

---

## Common Commands

**Build, test, and lint:**
```bash
cargo build --release
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

**Install and uninstall:**
```bash
./install.sh
./uninstall.sh
./uninstall.sh --yes
./uninstall.sh --keep-data
```

**BATS tests:**
```bash
bats tests/
```

**Shell syntax checks:**
```bash
bash -n scripts/*.sh install.sh setup.sh uninstall.sh
```

**Manual CLI operations:**
```bash
magi redis start
magi team create <team>
magi invite create --team <team> [--ttl 24h]
magi join --invite <token>
magi send <agent> <message>
magi inbox
magi history [--team <team>] [--agent <agent>]
magi team members [--team <team>]
magi watch [--format line|json]
magi config get <key>
magi config set <key> <value>
```

---

## Review Process

Before reporting completion for code changes:

1. Run the most relevant checks:
   - `cargo test` for CLI behavior
   - `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings`
   - `bats tests/` and `bash -n scripts/*.sh install.sh setup.sh uninstall.sh`
     for the retired stubs and shell entry points
2. Review synchronization:
   - CLI behavior matches `SKILL.md`
   - User-facing commands match `README.md`
   - Architecture details match `docs/design.md`
   - Claude Code and Codex templates remain consistent
3. Check repository instructions:
   - `CLAUDE.md` and `AGENTS.md` differ only by approved substitutions
   - No direct edits to real `~/.magi`/Redis state or installed skill files
   - No unrelated working-tree changes were modified

---

## Additional Instructions

@CLAUDE.local.md - Project-specific local preferences, if present.

**Note**: This CLAUDE.md focuses on core project rules and quick reference.
Consult `README.md`, `docs/design.md`, and `SKILL.md` for detailed behavior.
