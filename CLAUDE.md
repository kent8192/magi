# CLAUDE.md

## Purpose

This file contains project-specific instructions for the magi/agmsg project.
These rules keep agent work scoped, testable, and consistent across the Bash
scripts, SQLite message store, templates, and documentation.

For project behavior and architecture, see `README.md`, `docs/design.md`, and
`SKILL.md`.

---

## Project Overview

`agmsg` provides cross-agent messaging for CLI AI agents through a shared local
SQLite database. It has no daemon and no network service; agents interact with
the system through shell scripts installed under `~/.agents/skills/<cmd>/`.

**Repository URL**: https://github.com/kent8192/magi

---

## Tech Stack

- **Language**: Bash shell scripts
- **Storage**: SQLite with WAL mode
- **Tests**: BATS test suite under `tests/`
- **Install targets**: Claude Code slash command and Codex skill metadata
- **Templates**: `templates/cmd.claude-code.md` and `templates/cmd.codex.md`
- **Documentation**: `README.md`, `docs/design.md`, and `SKILL.md`

---

## Critical Rules

### Data and Configuration Access

**MUST use the provided scripts for all project data operations.**

- Use `scripts/join.sh` to join a team; there is no `register.sh`.
- Use `scripts/send.sh`, `scripts/inbox.sh`, `scripts/history.sh`, and
  `scripts/team.sh` for message and team operations.
- Use `scripts/delivery.sh` or `scripts/hook.sh` for delivery-mode changes.
- Do not directly edit SQLite database files, team `config.json` files, or
  generated installed skill files unless the task is explicitly about the
  installer or migration behavior.
- Preserve the no-daemon, no-network architecture.

### Code Style

- **ALL code comments MUST be written in English**.
- Keep scripts POSIX-aware where practical, but preserve the existing Bash
  contract when arrays, `[[ ... ]]`, or `set -euo pipefail` are already used.
- Prefer explicit error messages and deterministic exit codes in scripts.
- Keep dependencies limited to tools already used by the project: Bash,
  `sqlite3`, `awk`, `sed`, and standard Unix utilities.
- Do not introduce Python, Node.js, or network dependencies for core behavior.
- Mark placeholders with `TODO:` only when they represent intentional future
  work; remove obsolete TODOs when implementing the behavior.

### File Management

- Do not save temporary files in the project directory; use `/tmp` or
  `mktemp -d` and clean up.
- Delete backup files (`.bak`, `.backup`, `.old`, `~`) when no longer needed.
- Do not modify user configuration files such as `~/.codex/config.toml`
  directly from repository work. Provide commands or templates instead.
- Preserve unrelated working-tree changes, including local editor/tooling state.

### Testing

- New or changed script behavior should have focused BATS coverage when
  practical.
- Tests must use isolated temporary skill directories and must not read or write
  real `~/.agents/skills/<cmd>/db` or team state.
- Prefer meaningful assertions over output smoke checks.
- Run shell syntax checks for changed scripts.

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

- Do not commit, push, or open pull requests unless explicitly instructed.
- Keep changes scoped to the requested behavior.
- Split commits by specific intent if a commit is requested.
- Never use destructive Git operations such as `git reset --hard`, force-push,
  or branch deletion without explicit authorization.
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

**Install and update:**
```bash
./install.sh
./install.sh --cmd m
./install.sh --update
```

**Uninstall:**
```bash
./uninstall.sh
./uninstall.sh --yes
./uninstall.sh --keep-data
```

**Tests:**
```bash
bats tests/
```

**Shell syntax checks:**
```bash
bash -n scripts/*.sh install.sh setup.sh uninstall.sh
```

**Manual script operations:**
```bash
scripts/join.sh <team> <agent_name> <type> "$(pwd)"
scripts/whoami.sh "$(pwd)" <type>
scripts/send.sh <team> <from_agent> <to_agent> "<message>"
scripts/inbox.sh <team> <agent_id>
scripts/history.sh <team> [agent_id] [limit]
scripts/team.sh <team>
scripts/delivery.sh status [<type> <project_path>]
```

---

## Review Process

Before reporting completion for code changes:

1. Run the most relevant tests:
   - `bats tests/` for script behavior
   - `bash -n scripts/*.sh install.sh setup.sh uninstall.sh` for shell syntax
2. Review synchronization:
   - Script behavior matches `SKILL.md`
   - User-facing commands match `README.md`
   - Architecture details match `docs/design.md`
   - Claude Code and Codex templates remain consistent
3. Check repository instructions:
   - `CLAUDE.md` and `AGENTS.md` differ only by approved substitutions
   - No direct edits to real user DB/team/config state
   - No unrelated working-tree changes were modified

---

## Additional Instructions

@CLAUDE.local.md - Project-specific local preferences, if present.

**Note**: This CLAUDE.md focuses on core project rules and quick reference.
Consult `README.md`, `docs/design.md`, and `SKILL.md` for detailed behavior.
