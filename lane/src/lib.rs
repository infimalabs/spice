//! The supervised seat: one agent occupant per lane.
//!
//! A lane is a seat, not a process. The seat outlives the occupant sitting in
//! it, which is why identity, worktree, lineage, and epoch are durable state
//! while the occupant is a process group that comes and goes beneath them.
//! Every module here reduces facts into that durable state or guards a
//! transition of it; none of them decide when to run, because a supervisor that
//! reads a clock cannot be replayed.
//!
//! # Resident lifecycle ownership
//!
//! Rust owns the prototype's nine automatic wake invariants without moving a
//! clock into this replayable crate. `spice/src/resident/lifecycle.rs` is the
//! single process authority; durable bus commits and explicit `TaskWake` and
//! `LaneResume` timer events call it, and this crate continues to receive time
//! only as data. The complete ownership map is:
//!
//! - `LC-04`, `LC-08`, and `LC-09`: the resident event loop executes one
//!   capacity/select/claim/start decision at a time. The board's sole selector,
//!   allocation guard, and claim mint remain the allocator authority; a lane
//!   waiting on its retry deadline does not block a ready sibling lane.
//! - `LC-05`, `LC-06`, and `LC-07`: the lifecycle resolver counts selectable
//!   unclaimed tasks and derives the two-READY, lone-task starvation, and burst
//!   settling deadlines from the durable workflow-head transition facts.
//! - `LC-16` and `LC-17`: all three ordered arms share one per-lane backoff;
//!   authority movement gets a bounded retry, permanent local preconditions
//!   park until same-lane activity, and every refusal reports its closed trigger.
//! - `LC-18`: every arm enters the same resident launch path. Rapid-death hold
//!   decisions remain pure here in [`hold`], while the resident supplies their
//!   observed deaths, current instants, and durable hold deadlines.
//!
//! The resident owns clocks, timer scheduling, process launch, and retries.
//! This crate still owns only replayable reductions over explicit inputs, so
//! identical fact streams and supplied instants produce identical lane state.
//!
//! The crate sits directly above journal, vendor, tape, listen, atomic, sill,
//! and wire. It does not declare unrelated resident control-plane crates merely
//! to order the dependency lattice; `spice` composes those peers at the process
//! boundary.
//!
//! Time arrives as an argument. [`hold`] compares deadlines it was handed and
//! [`renew`] is told whether an occupant is alive; neither consults a clock, so
//! the same facts read in the same order always reduce to the same seat.

// Accounting is a guarded journal write, and journal gates its whole writer
// vocabulary out of the browser build. Every module reaching that vocabulary
// carries the same gate instead of asking the wasm target to compile a path it
// can never take; reaching only [`journal::crossing`] needs no gate, because
// the crossing types cross every plane by construction.
#[cfg(not(target_arch = "wasm32"))]
pub mod account;
pub mod capture;
pub mod config;
pub mod drain;
pub mod hold;
// Ingestion folds a tape into one accounted batch through [`account`], so it
// holds the writer vocabulary too and follows it behind the same gate.
#[cfg(not(target_arch = "wasm32"))]
pub mod ingest;
pub mod life;
pub mod occupant;
pub mod renew;
pub mod table;

pub use config::Config;
