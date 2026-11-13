//! Capability-scoped panes: what a principal sees and what it is offered.
//!
//! A pane is an affordance, not an authority. The resident authenticates every
//! command it receives and refuses one this layer would happily have built, so
//! nothing here is a security boundary. What a pane prevents is a misleading
//! control: an occupant is never shown a button whose only possible answer is
//! a refusal.
//!
//! A [`SteerPane`] builds ordinary commands for one [`Principal`] and offers
//! a control only where the command's wire law row admits that principal. It
//! borrows the principal it was opened for, so no pane outlives the
//! acceptance that produced it, and a [`Principal`] exists only by way of a
//! frame the resident sent and this session admitted.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use wire::principal::PrincipalRole;
use wire::{
    Accepted, Actor, Authority, CanonicalUuid, Command, CommandKind, LawError, WireVersion, Witness,
};

use crate::socket::{Admitted, SessionScope};

/// Who this session acts as, with the roles the resident last reported for it,
/// in the generation it acts in.
///
/// An actor is bound to the unrendered session scope that admitted it, so the
/// two travel together here as well. There is no constructor from text, from
/// a bare [`Actor`], or from anything the app rendered. The only door is
/// [`Principal::admitted`], which needs a frame the resident sent and
/// [`Session::admit_accepted`] let through. Current roles then come only from
/// the connected-readable principal snapshot; neither source lets caller text
/// grant authority.
///
/// [`Session::admit_accepted`]: crate::Session::admit_accepted
#[derive(Clone, PartialEq, Eq)]
pub struct Principal {
    actor: Actor,
    roles: BTreeSet<PrincipalRole>,
    scope: SessionScope,
}

impl fmt::Debug for Principal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Principal")
            .field("actor", &self.actor)
            .field("roles", &self.roles)
            .field("epoch", &self.epoch())
            .finish()
    }
}

impl Principal {
    /// Adopt the principal the resident derived for this session.
    #[must_use]
    pub fn admitted(accepted: &Admitted<Accepted>) -> Self {
        let scope = accepted.scope();
        let accepted = accepted.get();
        Self {
            roles: BTreeSet::new(),
            actor: accepted.actor.clone(),
            scope,
        }
    }

    /// The actor this session acts as.
    #[must_use]
    pub const fn actor(&self) -> &Actor {
        &self.actor
    }

    /// Replace current roles from one authority snapshot, returning whether
    /// this principal's command affordances changed.
    pub(crate) fn apply_roles(&mut self, roles: &BTreeSet<PrincipalRole>) -> bool {
        let changed = &self.roles != roles;
        self.roles.clone_from(roles);
        changed
    }

    /// Whether this principal currently holds operator authority.
    #[must_use]
    pub fn is_operator(&self) -> bool {
        self.roles.contains(&PrincipalRole::Operator)
    }

    /// The generation this principal was accepted into.
    #[must_use]
    pub const fn epoch(&self) -> CanonicalUuid {
        self.scope.epoch()
    }

    pub(crate) const fn scope(&self) -> SessionScope {
        self.scope
    }

    /// True when this principal is a durable external principal.
    #[must_use]
    pub const fn is_principal(&self) -> bool {
        matches!(self.actor, wire::Actor::Principal(_))
    }
}

/// Why a pane did not build the command it was asked for.
///
/// This is not a [`wire::Refusal`]. A refusal answers a command the resident
/// evaluated; nothing named here ever crossed the socket, so reporting it as
/// a refusal would put words in the resident's mouth.
#[derive(Debug, PartialEq, Eq)]
pub enum Declined {
    /// The command requires the operator capability and this principal is
    /// lane-scoped.
    Authority {
        /// Command that was asked for.
        kind: CommandKind,
        /// Principal that asked for it.
        actor: Actor,
    },
    /// The witnesses do not satisfy the command's law.
    Law(LawError),
}

impl fmt::Display for Declined {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authority { kind, actor } => write!(
                formatter,
                "{kind} requires the operator capability; this session acts as {actor}"
            ),
            Self::Law(error) => error.fmt(formatter),
        }
    }
}

impl Error for Declined {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Authority { .. } => None,
            Self::Law(error) => Some(error),
        }
    }
}

/// A pane that builds ordinary commands for one principal.
#[derive(Debug)]
pub struct SteerPane<'a> {
    principal: &'a Principal,
}

impl<'a> SteerPane<'a> {
    /// Open a steer pane for this principal.
    #[must_use]
    pub const fn over(principal: &'a Principal) -> Self {
        Self { principal }
    }

    /// True when this pane offers a control for `kind`.
    ///
    /// Hiding a control this principal cannot exercise removes a misleading
    /// affordance. It does not remove the command: only the resident decides
    /// that, and it decides the same way whether or not a control was drawn.
    ///
    /// The requirement is read from the command's registry row rather than
    /// restated here, so this pane cannot come to disagree with the law the
    /// resident will apply.
    #[must_use]
    pub fn offers(&self, kind: CommandKind) -> bool {
        match kind.law().authority {
            Authority::Connected => true,
            Authority::Operator => self.principal.is_operator(),
        }
    }

    /// Build one command at the version this build speaks.
    ///
    /// The command is witnessed before it is returned, so a caller cannot hold
    /// one this pane built that would lose a race it does not depend on.
    ///
    /// # Errors
    ///
    /// Returns [`Declined::Authority`] when this pane offers no control for
    /// the command, and [`Declined::Law`] when the witnesses do not satisfy
    /// the command's registry row.
    pub fn command<B>(
        &self,
        id: CanonicalUuid,
        kind: CommandKind,
        expected: Vec<Witness>,
        body: B,
    ) -> Result<Command<B>, Declined> {
        if !self.offers(kind) {
            return Err(Declined::Authority {
                kind,
                actor: self.principal.actor().clone(),
            });
        }
        let command = Command {
            v: WireVersion::CURRENT,
            id,
            epoch: self.principal.epoch(),
            kind,
            expected,
            body,
        };
        command.witnessed().map_err(Declined::Law)?;
        Ok(command)
    }
}
