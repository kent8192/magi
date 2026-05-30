//! `magi` — Redis-backed cross-agent messaging library for CLI AI agents.
//!
//! This crate is the root of the `magi` library.  It re-exports every
//! subsystem as a public module so that both the binary entry-point
//! (`src/main.rs`) and integration tests can reach them through a single,
//! stable namespace.
//!
//! # Module layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `cli` | Clap-based command-line interface definitions and dispatch |
//! | `config` | Runtime configuration (env vars, `~/.magi` state directory) |
//! | `error` | Unified `Error` / `Result` types for the whole crate |
//! | `install` | Binary installer: copies artefacts to `~/.agents/skills/magi/bin/` and `~/.local/bin/` |
//! | `invite` | Invite-code generation and redemption for onboarding new agents |
//! | `messaging` | Core messaging layer: Redis Streams for durable delivery, Pub/Sub for real-time fanout |
//! | `model` | Shared domain types (messages, teams, agents, invite codes) |
//! | `proc` | Managed embedded Redis server lifecycle (spawn, health-check, teardown) |
//! | `redis_client` | Thin async Redis client wrapper used throughout the crate |
//! | `redis_manager` | High-level manager that owns the connection pool and Stream/Pub-Sub multiplexing |
//! | `repl` | Interactive REPL for composing and reading messages at the terminal |
//! | `ssh` | SSH helpers for tunnelling connections to remote Redis instances |
//! | `team` | Team membership management: join, leave, list members |
//! | `watch` | Watch mode: continuous message tailing with line or JSON output |

/// CLI command definitions and top-level dispatch logic.
pub mod cli;

/// Runtime configuration sourced from environment variables and the
/// `~/.magi` state directory.
pub mod config;

/// Unified error and result types shared across all crate modules.
pub mod error;

/// Installer: copies the `magi` binary and skill wrappers into
/// `~/.agents/skills/magi/bin/magi` and `~/.local/bin/magi`.
pub mod install;

/// Invite-code based onboarding: generate single-use codes and redeem them
/// to admit a new agent into a team.
pub mod invite;

/// Core messaging subsystem built on Redis Streams (durable, ordered
/// delivery) and Pub/Sub (low-latency broadcast).
pub mod messaging;

/// Shared domain model types: messages, agent identities, team records,
/// and invite-code payloads.
pub mod model;

/// Managed embedded Redis server lifecycle — spawn, wait for readiness,
/// and graceful teardown, with optional process supervision.
pub mod proc;

/// Async Redis client wrapper providing connection management and
/// helpers for Stream and Pub/Sub operations.
pub mod redis_client;

/// High-level Redis manager that owns the connection pool and
/// coordinates Stream consumption with Pub/Sub multiplexing.
pub mod redis_manager;

/// Interactive REPL for composing outgoing messages and displaying
/// incoming ones at an interactive terminal session.
pub mod repl;

/// SSH tunnel helpers for forwarding local ports to a remote Redis
/// instance when agents run on different hosts.
pub mod ssh;

/// Team membership operations: create teams, add or remove members,
/// and enumerate current membership.
pub mod team;

/// Watch mode: tail a message stream continuously and emit each
/// message as a plain line or as structured JSON output.
pub mod watch;
