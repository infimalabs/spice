//! The sentences that mean "registered, but the work is not here yet".
//!
//! Reasons are inline literals nearly everywhere in this crate, because a
//! reason is read where it is emitted. These four are the exception: the
//! binary rim and the resident both emit them, and they are read from a
//! distance as absences of the work a leaf names, so each sentence is stated
//! once here and every emitter reads it.

/// One reason a registered leaf reports instead of the work it names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaceholderReason {
    /// A current public task owns this leaf's visible terminal refusal.
    NotLanded,
    /// A leaf registered as landed reached the terminal release branch.
    DispatchGap,
    /// A registered leaf states no landing disposition at all.
    LandingUnaccounted,
    /// The resident holds no handler for a registered command kind.
    HandlerNotLanded,
}

impl PlaceholderReason {
    /// The exact sentence this reason is emitted as.
    ///
    /// The match is exhaustive, so a reason added without a stated sentence
    /// fails to compile: the text is declared beside the variant rather than
    /// derived from its name, because the two are allowed to differ and the
    /// operator only ever sees the text.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotLanded => "release::not-landed",
            Self::DispatchGap => "release::dispatch-gap",
            Self::LandingUnaccounted => "release::landing-unaccounted",
            Self::HandlerNotLanded => "command::handler-not-landed",
        }
    }

    /// The owned spelling a `Refusal` or `Fault` carries.
    #[must_use]
    pub fn to_reason(self) -> String {
        self.as_str().to_owned()
    }
}
