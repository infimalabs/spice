use super::*;

#[macro_use]
mod macros;
mod reference_transaction;

mod registry;

pub(super) const CONTRACTS: &[Contract] = registry::CONTRACTS;
