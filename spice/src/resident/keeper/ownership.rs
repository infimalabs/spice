//! Whether lifecycle state this process would write belongs to the repository
//! user.
//!
//! The process's creation owner, the repository authority, and any existing
//! runtime root must name one principal. Foreign state is refused rather than
//! repaired or taken over.

/// Principal identities that decide who owns lifecycle state.
pub(super) struct Owners<'a> {
    /// The effective process user.
    pub(super) user: &'a str,
    /// The owner of every object this process would create.
    pub(super) default_owner: &'a str,
    /// Owner of the repository authority.
    pub(super) repository: Option<&'a str>,
    /// Owner of the existing runtime root; `None` when it is absent.
    pub(super) runtime: Option<&'a str>,
}

/// The principal that would own, or already owns, lifecycle state in place of
/// the effective process user.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Mismatch<'a> {
    /// New state would belong to this default owner.
    DefaultOwner(&'a str),
    /// The repository authority belongs to this principal.
    Repository(&'a str),
    /// The existing runtime root belongs to this principal.
    Runtime(&'a str),
}

/// The first ownership divergence, or `None` when every object the lifecycle
/// writes or reuses belongs to the effective process user.
pub(super) fn mismatch<'a>(owners: &Owners<'a>) -> Option<Mismatch<'a>> {
    if owners.default_owner != owners.user {
        return Some(Mismatch::DefaultOwner(owners.default_owner));
    }
    if let Some(repository) = owners.repository.filter(|owner| *owner != owners.user) {
        return Some(Mismatch::Repository(repository));
    }
    owners
        .runtime
        .filter(|runtime| *runtime != owners.user)
        .map(Mismatch::Runtime)
}
