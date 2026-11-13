//! One current read owned by the session scope that admitted it.

use wire::Accepted;

use crate::pane::Principal;
use crate::socket::{Admitted, SessionScope};

/// A snapshot a principal of another scope can neither read nor replace.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ScopedValue<T> {
    scope: SessionScope,
    pub(super) value: Option<T>,
}

impl<T> ScopedValue<T> {
    pub(super) fn new(accepted: &Admitted<Accepted>) -> Self {
        Self {
            scope: accepted.scope(),
            value: None,
        }
    }

    pub(super) fn get(&self, principal: &Principal) -> Option<&T> {
        (self.scope == principal.scope())
            .then_some(self.value.as_ref())
            .flatten()
    }

    pub(super) fn replace(&mut self, scope: SessionScope, value: T) -> bool {
        if self.scope != scope {
            return false;
        }
        self.value = Some(value);
        true
    }
}
