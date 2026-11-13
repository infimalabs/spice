//! The principal listing an operator-role principal manages from the app.

use wire::principal::PrincipalsAnswer;
use wire::{Outcome, Response};

use super::AppRuntime;
use crate::socket::Admitted;

impl AppRuntime {
    /// Every born principal, or the reason the resident refused this
    /// principal the listing; absent until the first read answers.
    #[must_use]
    pub fn principal_listing(&self) -> Option<Result<&PrincipalsAnswer, &str>> {
        self.principal_listing
            .get(&self.principal)
            .map(|listing| listing.as_ref().map_err(String::as_str))
    }

    /// Replace the listing with a read's answer or refusal.
    pub fn apply_principal_listing(&mut self, admitted: &Admitted<Response<PrincipalsAnswer>>) {
        let listing = match &admitted.get().outcome {
            Outcome::Reply(reply) => Ok(reply.body.clone()),
            Outcome::Refusal(refusal) => Err(refusal
                .diagnostic()
                .map_or("principal::list-refused", wire::RefusalDiagnostic::reason)
                .to_owned()),
            Outcome::Fault(fault) => Err(fault.reason.clone()),
        };
        self.principal_listing.replace(admitted.scope(), listing);
    }
}
