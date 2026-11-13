//! Pure task-document classification and canonical ledger emission.
//!
//! This crate deliberately performs no I/O. Callers provide document bytes and
//! optional family identity, inspect a [`Classification`], and decide whether a
//! later authority-owning layer may act on the resulting graph.

mod classifier;
mod ledger;
mod node;
mod refuse;

pub use classifier::{
    Classification, ClassificationContext, Warning, WarningCode, classify, classify_with_context,
};
pub use ledger::emit;
pub use node::{Document, Edge, EdgeKind, Node, NodeKind, Phase, Priority, slugify};
pub use refuse::{Refusal, RefusalCode};
