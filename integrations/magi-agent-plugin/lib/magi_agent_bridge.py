#!/usr/bin/env python3
"""Event-driven bridge between magi and a persistent Claude Agent SDK session.

This is the "Plan A" architecture: a long-lived process that subscribes to the
magi message stream and, the moment a message arrives, feeds its body into a
persistent Claude conversation as a new user turn. The assistant's reply is
then sent back to the originating agent through the magi CLI.

Design
------
* Input is taken from ``magi watch --format json`` (Redis Pub/Sub, instant,
  newline-delimited JSON). Reusing the CLI means this process never needs the
  Redis URL or credentials directly.
* Output (replies) is produced with ``magi send <peer> -- <reply>``.
* Each remote peer (the ``from`` field) gets its OWN persistent
  ``ClaudeSDKClient`` so conversations stay separated and keep their context
  across messages. Clients are created lazily and capped (LRU eviction).
* Incoming messages are processed strictly serially through an asyncio queue.
  This keeps the per-turn ``receive_response()`` boundary unambiguous and
  prevents an unbounded fan-out of concurrent Claude turns.

Safety / guardrails
-------------------
* Loop prevention: messages whose ``from`` equals our own agent name are
  always ignored (otherwise the agent would answer its own replies forever).
* Scope: only messages addressed to us (``to == self``) or, when
  ``MAGI_AGENT_SCOPE=team``, to our active team are handled.
* Optional sender allowlist (``MAGI_AGENT_ALLOW_FROM``).
* Tools are DISABLED by default (``allowed_tools=[]``) so the agent is a pure
  conversational responder and never runs commands unattended. Enabling tools
  is an explicit opt-in via ``MAGI_AGENT_ALLOWED_TOOLS``.

All configuration is read from environment variables; see ``_load_config``.
"""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import signal
import sys
from collections import OrderedDict
from dataclasses import dataclass

from claude_agent_sdk import (
    AssistantMessage,
    ClaudeAgentOptions,
    ClaudeSDKClient,
    TextBlock,
)

DEFAULT_SYSTEM_PROMPT = (
    "You are an autonomous teammate reachable over the `magi` cross-agent "
    "messaging system. Each user turn is a message another agent sent to you. "
    "Reply concisely and helpfully with plain text; your reply is delivered "
    "verbatim back to the sender, so do not wrap it in code fences or markdown "
    "scaffolding unless it genuinely helps. If a message needs no reply, answer "
    "with an empty response."
)


def _log(message: str) -> None:
    """Write a timestamped line to stdout (the launcher redirects it to a log)."""
    # Avoid importing datetime for a wall clock; rely on the supervisor/log
    # framework for timestamps. Prefix keeps lines greppable.
    print(f"[magi-agent] {message}", flush=True)


@dataclass
class Config:
    magi_bin: str
    self_agent: str
    self_team: str
    scope: str  # "direct" or "team"
    auto_reply: bool
    allow_from: frozenset[str]
    system_prompt: str
    allowed_tools: list[str]
    permission_mode: str
    model: str | None
    max_reply_chars: int
    max_peers: int
    cwd: str
    cli_path: str | None
    setting_sources: list[str] | None


def _resolve_magi_bin() -> str:
    """Locate the magi binary, honouring MAGI_BIN then PATH then the installer path."""
    explicit = os.environ.get("MAGI_BIN")
    if explicit:
        return explicit
    found = shutil.which("magi")
    if found:
        return found
    fallback = os.path.expanduser("~/.agents/skills/magi/bin/magi")
    if os.path.exists(fallback):
        return fallback
    return os.path.expanduser("~/.local/bin/magi")


def _magi_config_get(magi_bin: str, key: str) -> str:
    """Read a single magi config value synchronously (used only at startup)."""
    import subprocess

    try:
        out = subprocess.run(
            [magi_bin, "config", "get", key],
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError) as exc:  # pragma: no cover - startup guard
        raise SystemExit(f"failed to run `{magi_bin} config get {key}`: {exc}")
    if out.returncode != 0:
        # An unset identity key returns empty rather than error in magi, but be
        # defensive about other failures.
        return ""
    return out.stdout.strip()


def _load_config() -> Config:
    magi_bin = _resolve_magi_bin()
    self_agent = os.environ.get("MAGI_AGENT_SELF") or _magi_config_get(
        magi_bin, "identity.active_agent"
    )
    self_team = os.environ.get("MAGI_AGENT_TEAM") or _magi_config_get(
        magi_bin, "identity.active_team"
    )
    if not self_agent:
        raise SystemExit(
            "identity.active_agent is not set; run "
            "`magi config set identity.active_agent <name>` first"
        )

    scope = os.environ.get("MAGI_AGENT_SCOPE", "direct").strip().lower()
    if scope not in ("direct", "team"):
        scope = "direct"

    allow_from_raw = os.environ.get("MAGI_AGENT_ALLOW_FROM", "").strip()
    allow_from = frozenset(
        item.strip() for item in allow_from_raw.split(",") if item.strip()
    )

    allowed_tools_raw = os.environ.get("MAGI_AGENT_ALLOWED_TOOLS", "").strip()
    allowed_tools = [t.strip() for t in allowed_tools_raw.split(",") if t.strip()]

    # By default run as a lean responder that does NOT inherit the user's global
    # CLAUDE.md / project settings, so the bot's behaviour is predictable. Set
    # MAGI_AGENT_SETTING_SOURCES (e.g. "user,project") to opt back in.
    sources_raw = os.environ.get("MAGI_AGENT_SETTING_SOURCES")
    if sources_raw is None:
        setting_sources: list[str] | None = []
    else:
        setting_sources = [s.strip() for s in sources_raw.split(",") if s.strip()]

    return Config(
        magi_bin=magi_bin,
        self_agent=self_agent,
        self_team=self_team,
        scope=scope,
        auto_reply=os.environ.get("MAGI_AGENT_AUTO_REPLY", "1") not in ("0", "false", "no"),
        allow_from=allow_from,
        system_prompt=os.environ.get("MAGI_AGENT_SYSTEM_PROMPT", DEFAULT_SYSTEM_PROMPT),
        allowed_tools=allowed_tools,
        permission_mode=os.environ.get("MAGI_AGENT_PERMISSION_MODE", "default"),
        model=os.environ.get("MAGI_AGENT_MODEL") or None,
        max_reply_chars=int(os.environ.get("MAGI_AGENT_MAX_REPLY_CHARS", "4000")),
        max_peers=int(os.environ.get("MAGI_AGENT_MAX_PEERS", "8")),
        cwd=os.environ.get("MAGI_AGENT_CWD") or os.path.expanduser("~"),
        cli_path=shutil.which("claude"),
        setting_sources=setting_sources,
    )


class PeerSessions:
    """Lazily-created, LRU-capped pool of persistent Claude clients keyed by peer."""

    def __init__(self, config: Config) -> None:
        self._config = config
        self._clients: OrderedDict[str, ClaudeSDKClient] = OrderedDict()

    def _new_options(self) -> ClaudeAgentOptions:
        # Build options fresh per client so each peer is an independent session.
        return ClaudeAgentOptions(
            system_prompt=self._config.system_prompt,
            allowed_tools=self._config.allowed_tools,
            permission_mode=self._config.permission_mode,  # type: ignore[arg-type]
            model=self._config.model,
            cwd=self._config.cwd,
            cli_path=self._config.cli_path,
            setting_sources=self._config.setting_sources,  # type: ignore[arg-type]
        )

    async def get(self, peer: str) -> ClaudeSDKClient:
        client = self._clients.get(peer)
        if client is not None:
            self._clients.move_to_end(peer)
            return client

        # Evict the least-recently-used peer when at capacity.
        while len(self._clients) >= self._config.max_peers:
            old_peer, old_client = self._clients.popitem(last=False)
            _log(f"evicting LRU session for peer={old_peer}")
            await _safe_disconnect(old_client)

        client = ClaudeSDKClient(options=self._new_options())
        await client.connect()
        self._clients[peer] = client
        _log(f"opened persistent session for peer={peer}")
        return client

    async def close_all(self) -> None:
        for peer, client in list(self._clients.items()):
            await _safe_disconnect(client)
        self._clients.clear()


async def _safe_disconnect(client: ClaudeSDKClient) -> None:
    try:
        await client.disconnect()
    except Exception as exc:  # noqa: BLE001 - best-effort cleanup
        _log(f"error during disconnect (ignored): {exc}")


async def _collect_reply(client: ClaudeSDKClient) -> str:
    """Drain one assistant turn and concatenate its text blocks."""
    parts: list[str] = []
    async for message in client.receive_response():
        if isinstance(message, AssistantMessage):
            for block in message.content:
                if isinstance(block, TextBlock):
                    parts.append(block.text)
        # receive_response() stops on its own after the turn's ResultMessage.
    return "".join(parts).strip()


async def _send_reply(config: Config, to: str, body: str) -> None:
    """Deliver a reply back through the magi CLI."""
    if config.max_reply_chars > 0 and len(body) > config.max_reply_chars:
        body = body[: config.max_reply_chars]
    # `--` guards against replies that begin with a dash being parsed as flags.
    proc = await asyncio.create_subprocess_exec(
        config.magi_bin,
        "send",
        to,
        "--",
        body,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    _, stderr = await proc.communicate()
    if proc.returncode != 0:
        _log(f"magi send to={to} failed rc={proc.returncode}: {stderr.decode().strip()}")
    else:
        _log(f"replied to peer={to} ({len(body)} chars)")


def _should_handle(config: Config, msg: dict) -> bool:
    sender = msg.get("from", "")
    to = msg.get("to", "")
    if not sender or sender == config.self_agent:
        return False  # loop prevention: never react to our own messages
    if config.allow_from and sender not in config.allow_from:
        return False
    if config.scope == "team":
        return to == config.self_agent or (bool(config.self_team) and to == config.self_team)
    return to == config.self_agent


async def _worker(config: Config, sessions: PeerSessions, queue: "asyncio.Queue[dict]") -> None:
    """Process incoming messages one at a time and reply via magi."""
    while True:
        msg = await queue.get()
        try:
            sender = msg.get("from", "")
            body = msg.get("body", "")
            _log(f"handling message id={msg.get('id')} from={sender}: {body[:80]!r}")
            client = await sessions.get(sender)
            await client.query(body)
            reply = await _collect_reply(client)
            if config.auto_reply and reply:
                await _send_reply(config, sender, reply)
            elif not reply:
                _log(f"empty reply for peer={sender}; nothing sent")
        except Exception as exc:  # noqa: BLE001 - keep the loop alive on per-message errors
            _log(f"error handling message id={msg.get('id')}: {exc}")
        finally:
            queue.task_done()


async def _reader(config: Config, queue: "asyncio.Queue[dict]", proc: asyncio.subprocess.Process) -> None:
    """Read NDJSON lines from `magi watch` and enqueue ones we should handle."""
    assert proc.stdout is not None
    async for raw in proc.stdout:
        line = raw.decode(errors="replace").strip()
        if not line:
            continue
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            _log(f"skipping non-JSON line: {line[:120]!r}")
            continue
        if _should_handle(config, msg):
            await queue.put(msg)
        else:
            _log(f"ignored id={msg.get('id')} from={msg.get('from')} to={msg.get('to')}")


async def run() -> int:
    config = _load_config()
    _log(
        f"starting bridge self={config.self_agent} team={config.self_team or '-'} "
        f"scope={config.scope} auto_reply={config.auto_reply} "
        f"tools={config.allowed_tools or 'none'} model={config.model or 'default'}"
    )

    sessions = PeerSessions(config)
    queue: "asyncio.Queue[dict]" = asyncio.Queue()

    proc = await asyncio.create_subprocess_exec(
        config.magi_bin,
        "watch",
        "--format",
        "json",
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        try:
            loop.add_signal_handler(sig, stop.set)
        except NotImplementedError:  # pragma: no cover - non-Unix
            pass

    worker_task = asyncio.create_task(_worker(config, sessions, queue))
    reader_task = asyncio.create_task(_reader(config, queue, proc))

    await stop.wait()
    _log("shutdown requested; cleaning up")

    reader_task.cancel()
    worker_task.cancel()
    for task in (reader_task, worker_task):
        try:
            await task
        except asyncio.CancelledError:
            pass

    if proc.returncode is None:
        proc.terminate()
        try:
            await asyncio.wait_for(proc.wait(), timeout=5)
        except asyncio.TimeoutError:
            proc.kill()
    await sessions.close_all()
    _log("stopped")
    return 0


def main() -> None:
    try:
        raise SystemExit(asyncio.run(run()))
    except KeyboardInterrupt:
        sys.exit(0)


if __name__ == "__main__":
    main()
