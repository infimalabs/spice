//! Durable outbound direction and the acknowledgment that retires it.
//!
//! Operator steering is state, not text that scrolled past. A directive is
//! appended once, rendered where the lane it addresses will hear it, and
//! retires only through a typed disposition: a structural acknowledgment in
//! assistant prose, an authenticated lane command, an operator cancel, or its
//! own expiry. Silence climbs a resend ladder and then alarms by name rather
//! than escalating forever.
//!
//! The modules follow that arc. [`ack`] fixes the key grammar and reads
//! outcomes out of prose with no I/O at all. [`directives`] reduces steering
//! facts into what every directive currently stands at. [`inbox`] renders what
//! every lane still owes through the publication primitive, and [`hook`] names
//! the lane and appliance identities carried into agent commands.
//!
//! The split by target is deliberate: the grammar and the reducer are pure and
//! compile everywhere, while the modules that touch a filesystem do not exist
//! on wasm32.

#![forbid(unsafe_code)]

pub mod ack;
pub mod config;
pub mod directives;
#[cfg(not(target_arch = "wasm32"))]
pub mod hook;
#[cfg(not(target_arch = "wasm32"))]
pub mod inbox;

pub use ack::keys::{
    DISPOSITIONS, DispositionError, Key, KeyError, KeyMintError, MARKERS, Marker, disposition,
};
pub use ack::scan::{
    Locus, Miss, Outcome, Recognition, Standing, ack_reaction, answer, decisive,
    has_acknowledgement, placed_headers, scan,
};
pub use config::{Config, WrapperReference, Wrappers};
pub use directives::{Directive, Ledger, Refusal, Steering};
