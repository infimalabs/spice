//! Connected-principal identity, authority, and hydration.

use wire::intent::InboxAnswer;
use wire::principal::{PrincipalDirectory, PrincipalDirectoryAnswer};
use wire::{Accepted, CommandKind, Response, ViewSubject};

use super::AppRuntime;
use crate::pane::{Principal, SteerPane};
use crate::socket::Admitted;

impl AppRuntime {
    /// Whether an acceptance belongs to the authority this runtime may retain.
    #[must_use]
    pub fn same_scope(&self, accepted: &Admitted<Accepted>) -> bool {
        self.accepted.scope() == accepted.scope()
    }

    /// Whether every current snapshot this principal may read is present.
    #[must_use]
    pub fn is_hydrated(&self) -> bool {
        self.board().is_some()
            && self.principals().is_some()
            && (!self.offers(CommandKind::LaneRead) || self.lanes().is_some())
            && (!self.offers(CommandKind::SteerInbox) || self.inbox().is_some())
            && (!self.offers(CommandKind::ConsentRead) || self.consent_queue().is_some())
            && (!self.offers(CommandKind::GlobalSettingsRead) || self.settings().is_some())
            && (!self.offers(CommandKind::VendorCatalog) || self.vendor_catalog().is_some())
    }

    #[must_use]
    pub const fn principal(&self) -> &Principal {
        &self.principal
    }

    /// Stream subjects admitted for this principal's current roles.
    #[must_use]
    pub fn subjects(&self) -> Vec<ViewSubject> {
        let mut subjects = vec![ViewSubject::Facts, ViewSubject::Rooms];
        if self.principal.is_operator() {
            subjects.push(ViewSubject::Messages);
            subjects.push(ViewSubject::Quota);
        }
        subjects
    }

    /// Whether the principal's steer pane offers this command.
    #[must_use]
    pub fn offers(&self, kind: CommandKind) -> bool {
        SteerPane::over(&self.principal).offers(kind)
    }

    /// Every born principal's display identity, absent until its dedicated
    /// connected-readable snapshot lands.
    #[must_use]
    pub fn principals(&self) -> Option<&PrincipalDirectory> {
        self.principals.get(&self.principal)
    }

    /// How the connected principal presents, absent until its directory lands.
    #[must_use]
    pub fn own_identity(&self) -> Option<&wire::principal::PrincipalDisplay> {
        self.principals()?.identity(self.principal.actor())
    }

    /// Apply every principal's current display identity and this connected
    /// principal's roles. Returns whether its command authority changed.
    pub fn apply_principals(
        &mut self,
        admitted: &Admitted<Response<PrincipalDirectoryAnswer>>,
    ) -> bool {
        let mut authority_changed = false;
        self.apply_response(admitted, |runtime, answer| {
            authority_changed = runtime.principal.apply_roles(&answer.roles);
            runtime
                .principals
                .replace(admitted.scope(), answer.principals.clone());
        });
        authority_changed
    }

    pub fn apply_inbox(&mut self, admitted: &Admitted<Response<InboxAnswer>>) {
        self.apply_response(admitted, |runtime, answer| {
            runtime.inbox.replace(admitted.scope(), answer.clone());
        });
    }
}
