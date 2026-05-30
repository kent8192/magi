/**
 * Event-driven bridge between magi and a persistent Claude Agent SDK session.
 *
 * "Plan A" architecture in TypeScript: a long-lived process that subscribes to
 * the magi message stream and, the instant a message arrives, feeds its body
 * into a persistent Claude conversation as a new user turn. The assistant reply
 * is then sent back to the originating agent through the magi CLI.
 *
 * Run directly with Node's native TypeScript type-stripping (Node >= 22.18):
 *   node lib/magi_agent_bridge.ts
 *
 * Design
 * ------
 * - Input comes from `magi watch --format json` (Redis Pub/Sub, instant NDJSON).
 *   Reusing the CLI means this process never needs Redis credentials.
 * - Output (replies) uses `magi send <peer> -- <reply>`.
 * - Each remote peer (the `from` field) gets its OWN persistent `query()`
 *   session driven by a pushable async-generator input, so conversations stay
 *   separated and keep context across messages. Sessions are LRU-capped.
 * - Incoming messages are processed strictly serially so each turn's response
 *   boundary (the `result` message) is unambiguous.
 *
 * Safety / guardrails
 * -------------------
 * - Loop prevention: a message whose `from` equals our own agent name is always
 *   ignored (otherwise the agent would answer its own replies forever).
 * - Scope: only messages addressed to us (or, in `team` scope, our active team).
 * - Optional sender allowlist (MAGI_AGENT_ALLOW_FROM).
 * - Tools are DISABLED by default: in the `default` permission mode a
 *   deny-by-default `canUseTool` guard denies any tool not in
 *   MAGI_AGENT_ALLOWED_TOOLS, so an unattended turn can never hang on approval.
 */

import { spawn } from "node:child_process";
import { execFileSync } from "node:child_process";
import { accessSync, constants } from "node:fs";
import { homedir } from "node:os";
import { delimiter, join } from "node:path";
import { createInterface } from "node:readline";
import process from "node:process";

import { query } from "@anthropic-ai/claude-agent-sdk";
import type {
  CanUseTool,
  Options,
  PermissionMode,
  SDKMessage,
  SDKUserMessage,
} from "@anthropic-ai/claude-agent-sdk";

const DEFAULT_SYSTEM_PROMPT =
  "You are an autonomous teammate reachable over the `magi` cross-agent " +
  "messaging system. Each user turn is a message another agent sent to you. " +
  "Reply concisely and helpfully with plain text; your reply is delivered " +
  "verbatim back to the sender, so do not wrap it in code fences or markdown " +
  "scaffolding unless it genuinely helps. If a message needs no reply, answer " +
  "with an empty response.";

function log(message: string): void {
  // The controller redirects stdout/stderr to the daemon log file.
  console.log(`[magi-agent] ${message}`);
}

type Config = {
  magiBin: string;
  selfAgent: string;
  selfTeam: string;
  scope: string;
  autoReply: boolean;
  allowFrom: Set<string>;
  systemPrompt: string;
  allowedTools: string[];
  permissionMode: PermissionMode;
  model: string | undefined;
  maxReplyChars: number;
  maxPeers: number;
  cwd: string;
  cliPath: string | undefined;
  settingSources: string[];
};

type MagiMessage = {
  id?: string;
  from?: string;
  to?: string;
  body?: string;
  created_at?: string;
};

function envTruthy(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined) return fallback;
  return !["0", "false", "no", "off"].includes(value.toLowerCase());
}

function which(bin: string): string | undefined {
  // Resolve an executable without spawning a shell (avoids the shell-injection
  // surface and Node's DEP0190 warning for shell:true with args).
  if (bin.includes("/")) {
    try {
      accessSync(bin, constants.X_OK);
      return bin;
    } catch {
      return undefined;
    }
  }
  for (const dir of (process.env.PATH || "").split(delimiter)) {
    if (!dir) continue;
    const full = join(dir, bin);
    try {
      accessSync(full, constants.X_OK);
      return full;
    } catch {
      // not in this directory; keep scanning
    }
  }
  return undefined;
}

function resolveMagiBin(): string {
  const explicit = process.env.MAGI_BIN;
  if (explicit) return explicit;
  const found = which("magi");
  if (found) return found;
  return `${homedir()}/.local/bin/magi`;
}

function magiConfigGet(magiBin: string, key: string): string {
  try {
    return execFileSync(magiBin, ["config", "get", key], { encoding: "utf8" }).trim();
  } catch {
    return "";
  }
}

function loadConfig(): Config {
  const magiBin = resolveMagiBin();
  const selfAgent = process.env.MAGI_AGENT_SELF || magiConfigGet(magiBin, "identity.active_agent");
  const selfTeam = process.env.MAGI_AGENT_TEAM || magiConfigGet(magiBin, "identity.active_team");
  if (!selfAgent) {
    throw new Error(
      "identity.active_agent is not set; run `magi config set identity.active_agent <name>` first",
    );
  }

  let scope = (process.env.MAGI_AGENT_SCOPE || "direct").trim().toLowerCase();
  if (scope !== "direct" && scope !== "team") scope = "direct";

  const allowFrom = new Set(
    (process.env.MAGI_AGENT_ALLOW_FROM || "")
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean),
  );

  const allowedTools = (process.env.MAGI_AGENT_ALLOWED_TOOLS || "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);

  // Lean by default: do NOT inherit the user's global CLAUDE.md / project
  // settings unless MAGI_AGENT_SETTING_SOURCES is set.
  const sourcesRaw = process.env.MAGI_AGENT_SETTING_SOURCES;
  const settingSources =
    sourcesRaw === undefined
      ? []
      : sourcesRaw.split(",").map((s) => s.trim()).filter(Boolean);

  return {
    magiBin,
    selfAgent,
    selfTeam,
    scope,
    autoReply: envTruthy(process.env.MAGI_AGENT_AUTO_REPLY, true),
    allowFrom,
    systemPrompt: process.env.MAGI_AGENT_SYSTEM_PROMPT || DEFAULT_SYSTEM_PROMPT,
    allowedTools,
    permissionMode: (process.env.MAGI_AGENT_PERMISSION_MODE || "default") as PermissionMode,
    model: process.env.MAGI_AGENT_MODEL || undefined,
    maxReplyChars: Number.parseInt(process.env.MAGI_AGENT_MAX_REPLY_CHARS || "4000", 10),
    maxPeers: Number.parseInt(process.env.MAGI_AGENT_MAX_PEERS || "8", 10),
    cwd: process.env.MAGI_AGENT_CWD || homedir(),
    cliPath: which("claude"),
    settingSources,
  };
}

/**
 * A deny-by-default permission guard. `allowedTools` only pre-approves the
 * tools you list; it does not deny the rest, so under `permissionMode:
 * "default"` an unlisted tool would fall through to interactive approval and an
 * unattended daemon would hang. This guard denies anything not explicitly
 * allowed, making "tools off by default" an enforced invariant.
 */
function makePermissionGuard(allowedTools: string[]): CanUseTool {
  const allowed = new Set(allowedTools);
  return async (toolName, input, _opts) => {
    if (allowed.has(toolName)) {
      return { behavior: "allow", updatedInput: input };
    }
    return {
      behavior: "deny",
      message:
        `magi-agent: tool '${toolName}' is not permitted for this unattended ` +
        "responder. Add it to MAGI_AGENT_ALLOWED_TOOLS to enable.",
    };
  };
}

function buildOptions(config: Config): Options {
  const options: Options = {
    systemPrompt: config.systemPrompt,
    allowedTools: config.allowedTools,
    permissionMode: config.permissionMode,
    model: config.model,
    cwd: config.cwd,
    settingSources: config.settingSources as Options["settingSources"],
    pathToClaudeCodeExecutable: config.cliPath,
  };
  // Only guard the default mode; an explicit mode is the user's deliberate
  // opt-in (e.g. acceptEdits / bypassPermissions) and is left untouched.
  if (config.permissionMode === "default") {
    options.canUseTool = makePermissionGuard(config.allowedTools);
  }
  return options;
}

/** A minimal pushable async iterable: values can be pushed over time. */
function makePushable<T>() {
  const queue: T[] = [];
  let wake: (() => void) | null = null;
  let ended = false;
  async function* generator(): AsyncGenerator<T> {
    while (true) {
      while (queue.length > 0) yield queue.shift() as T;
      if (ended) return;
      await new Promise<void>((resolve) => {
        wake = resolve;
      });
    }
  }
  return {
    iterable: generator(),
    push(value: T) {
      queue.push(value);
      const w = wake;
      wake = null;
      if (w) w();
    },
    end() {
      ended = true;
      const w = wake;
      wake = null;
      if (w) w();
    },
  };
}

/** One persistent Claude session bound to a single magi peer. */
class PeerSession {
  peer: string;
  input: ReturnType<typeof makePushable<SDKUserMessage>>;
  iterator: AsyncIterator<SDKMessage>;

  constructor(peer: string, options: Options) {
    this.peer = peer;
    this.input = makePushable<SDKUserMessage>();
    const q = query({ prompt: this.input.iterable, options });
    this.iterator = q[Symbol.asyncIterator]();
  }

  async ask(text: string): Promise<string> {
    const userMessage = {
      type: "user",
      message: { role: "user", content: text },
      parent_tool_use_id: null,
      session_id: this.peer,
    } as unknown as SDKUserMessage;
    this.input.push(userMessage);

    let assistantText = "";
    while (true) {
      const { value, done } = await this.iterator.next();
      if (done) break;
      const msg = value as SDKMessage;
      if (msg.type === "assistant") {
        const content = (msg as { message?: { content?: unknown } }).message?.content;
        if (Array.isArray(content)) {
          for (const block of content) {
            if (block && block.type === "text" && typeof block.text === "string") {
              assistantText += block.text;
            }
          }
        }
      } else if (msg.type === "result") {
        const result = (msg as { result?: string }).result;
        return (typeof result === "string" ? result : assistantText).trim();
      }
    }
    return assistantText.trim();
  }

  close(): void {
    this.input.end();
  }
}

class PeerSessions {
  config: Config;
  sessions: Map<string, PeerSession>;

  constructor(config: Config) {
    this.config = config;
    this.sessions = new Map();
  }

  get(peer: string): PeerSession {
    const existing = this.sessions.get(peer);
    if (existing) {
      // Refresh LRU order.
      this.sessions.delete(peer);
      this.sessions.set(peer, existing);
      return existing;
    }
    while (this.sessions.size >= this.config.maxPeers) {
      const oldest = this.sessions.keys().next().value as string | undefined;
      if (oldest === undefined) break;
      log(`evicting LRU session for peer=${oldest}`);
      this.sessions.get(oldest)?.close();
      this.sessions.delete(oldest);
    }
    const session = new PeerSession(peer, buildOptions(this.config));
    this.sessions.set(peer, session);
    log(`opened persistent session for peer=${peer}`);
    return session;
  }

  closeAll(): void {
    for (const session of this.sessions.values()) session.close();
    this.sessions.clear();
  }
}

function shouldHandle(config: Config, msg: MagiMessage): boolean {
  const sender = msg.from || "";
  const to = msg.to || "";
  if (!sender || sender === config.selfAgent) return false; // loop prevention
  if (config.allowFrom.size > 0 && !config.allowFrom.has(sender)) return false;
  if (config.scope === "team") {
    return to === config.selfAgent || (config.selfTeam !== "" && to === config.selfTeam);
  }
  return to === config.selfAgent;
}

function sendReply(config: Config, to: string, bodyIn: string): Promise<void> {
  let body = bodyIn;
  if (config.maxReplyChars > 0 && body.length > config.maxReplyChars) {
    body = body.slice(0, config.maxReplyChars);
  }
  return new Promise<void>((resolve) => {
    // `--` guards against replies that begin with a dash being parsed as flags.
    const child = spawn(config.magiBin, ["send", to, "--", body], { stdio: "ignore" });
    child.on("error", (err) => {
      log(`magi send to=${to} failed: ${err}`);
      resolve();
    });
    child.on("close", (code) => {
      if (code === 0) log(`replied to peer=${to} (${body.length} chars)`);
      else log(`magi send to=${to} exited code=${code}`);
      resolve();
    });
  });
}

async function main(): Promise<void> {
  const config = loadConfig();
  log(
    `starting bridge self=${config.selfAgent} team=${config.selfTeam || "-"} ` +
      `scope=${config.scope} auto_reply=${config.autoReply} ` +
      `tools=${config.allowedTools.length ? config.allowedTools.join(",") : "none"} ` +
      `model=${config.model || "default"}`,
  );

  const sessions = new PeerSessions(config);
  const queue = makePushable<MagiMessage>();
  let stopping = false;

  // Long-lived `magi watch` subprocess. Inherit stderr (the controller logs it)
  // rather than buffering an undrained pipe that could fill and stall.
  const watch = spawn(config.magiBin, ["watch", "--format", "json"], {
    stdio: ["ignore", "pipe", "inherit"],
  });

  const shutdown = (reason: string) => {
    if (stopping) return;
    stopping = true;
    log(`shutdown requested (${reason}); cleaning up`);
    queue.end();
    if (watch.exitCode === null) watch.kill("SIGTERM");
  };

  process.on("SIGINT", () => shutdown("SIGINT"));
  process.on("SIGTERM", () => shutdown("SIGTERM"));

  // Detect `magi watch` death (e.g. Redis unreachable) so the daemon exits
  // instead of lingering "RUNNING" while processing nothing.
  watch.on("close", (code) => shutdown(`magi watch exited code=${code}`));

  const rl = createInterface({ input: watch.stdout, crlfDelay: Infinity });
  rl.on("line", (line) => {
    const text = line.trim();
    if (!text) return;
    let msg: MagiMessage;
    try {
      msg = JSON.parse(text) as MagiMessage;
    } catch {
      log(`skipping non-JSON line: ${text.slice(0, 120)}`);
      return;
    }
    if (shouldHandle(config, msg)) queue.push(msg);
    else log(`ignored id=${msg.id} from=${msg.from} to=${msg.to}`);
  });

  // Serial worker: one turn at a time.
  for await (const msg of queue.iterable) {
    try {
      const sender = msg.from || "";
      const body = msg.body || "";
      log(`handling message id=${msg.id} from=${sender}: ${JSON.stringify(body.slice(0, 80))}`);
      const session = sessions.get(sender);
      const reply = await session.ask(body);
      if (config.autoReply && reply) await sendReply(config, sender, reply);
      else if (!reply) log(`empty reply for peer=${sender}; nothing sent`);
    } catch (err) {
      log(`error handling message id=${msg.id}: ${err}`);
    }
  }

  sessions.closeAll();
  log("stopped");
}

main().catch((err) => {
  log(`fatal: ${err && err.stack ? err.stack : err}`);
  process.exit(1);
});
