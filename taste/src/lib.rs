//! Configured prose candidates and live reminder identity.
//!
//! This crate selects candidates and mints their stable content and bag
//! identities. It deliberately owns no process: resident recognition decides
//! whether an optional adjudicator sits between a hit and ordinary steering,
//! while a disabled adjudicator preserves the direct deterministic path.

#![forbid(unsafe_code)]

mod bag;
mod config;

pub use bag::{
    Bag, BagError, Hit, MAX_JUDGE_SOURCE_BYTES, Span, Taste, Trigger, TriggerError, bag_identity,
    normalize_prose, reminder_identity,
};
pub use config::{Config, ConfigError, configured};
