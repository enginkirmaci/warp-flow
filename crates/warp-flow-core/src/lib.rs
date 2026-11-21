//! warp-flow-core — the engine.
//!
//! Three parts, in this order of trust:
//!
//! - the **issue folder protocol**: markdown under `.scratch/<feature-slug>/`
//!   is the only durable interface between steps (ADR-0009);
//! - the **workflow graph**: a declarative TOML graph of steps, transitions and
//!   gates, copied per issue and revised as it changes (ADR-0010);
//! - the **harness boundary**: a step is a session on warp-agent's door, never
//!   a model call of our own (ADR-0008, ADR-0011).
//!
//! This crate holds no UI and no model client.

/// The engine version, for the CLI and for `run.md`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
