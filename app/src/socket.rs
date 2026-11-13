//! The browser's transport boundary: one pre-socket clock and one frame door.
//!
//! The app carries a browser/resident protocol clock independent of durable
//! [`WireVersion`]. A page can survive an installed resident replacement, so
//! discovery admits its app clock before any command payload is allowed to
//! reach browser state. Nothing here decides authority; admission only decides
//! whether bytes may cross that door.

use std::fmt;

use serde::de::DeserializeOwned;
use wire::{
    Accepted, AppProtocolVersion, CanonicalUuid, Fault, FaultClass, Hello, Response, StreamFrame,
    WireVersion,
};

/// Reasons the pre-socket and frame doors name when they turn input away.
///
/// Both frame shapes draw from this one vocabulary. A second spelling for the
/// same failure would let a caller handle one door and miss the other.
mod reason {
    /// The bytes did not decode as the current DTO.
    pub const UNDECODABLE: &str = "frame::undecodable";
    /// The frame carried a version this build does not speak.
    pub const WIRE: &str = "frame::wire-mismatch";
    /// Bootstrap metadata belongs to another browser/resident protocol.
    pub const APP_PROTOCOL: &str = "bootstrap::app-protocol-mismatch";
    /// The frame answered a generation this session does not address.
    pub const EPOCH: &str = "frame::epoch-mismatch";
}

/// A frame that passed this session's door.
///
/// The inner frame is private and no constructor is public, so the only way to
/// hold one is to have been given it by a [`Session`]. The runtime accepts
/// this type rather than a bare DTO, which makes "apply only what was admitted"
/// the compiler checks instead of a discipline each caller must remember.
///
/// Forging an admission does not compile:
///
/// ```compile_fail
/// let forged = app::Admitted(7_u8);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admitted<T> {
    frame: T,
    scope: SessionScope,
}

impl<T> Admitted<T> {
    /// The frame this admission carries.
    #[must_use]
    pub const fn get(&self) -> &T {
        &self.frame
    }

    /// Take the frame out of its admission.
    ///
    /// Admission is a fact about bytes already checked, not a permission that
    /// can expire, so unwrapping loses only the proof and not a guarantee.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.frame
    }

    pub(crate) const fn scope(&self) -> SessionScope {
        self.scope
    }
}

/// Unrendered identity of the capability-scoped socket that admitted a frame.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionScope {
    capability: CanonicalUuid,
    epoch: CanonicalUuid,
}

impl SessionScope {
    /// The generation this scope is bound to.
    ///
    /// The scope already carries the generation, so state that is scoped to a
    /// principal has no reason to spell the generation a second time of its
    /// own. Reading it back through here is what keeps the two from being
    /// kept in agreement by hand.
    pub(crate) const fn epoch(&self) -> CanonicalUuid {
        self.epoch
    }
}

impl fmt::Debug for SessionScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SessionScope([redacted])")
    }
}

/// Decode bytes as the current DTO, reporting a failure rather than repairing.
fn decode_frame<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Fault> {
    wire::decode(bytes).map_err(|error| Fault {
        class: FaultClass::Protocol,
        reason: reason::UNDECODABLE.to_owned(),
        detail: error.to_string(),
    })
}

/// Admit a version against the one this build speaks, naming both on mismatch.
fn admit_wire(observed: WireVersion) -> Result<(), Fault> {
    if observed.is_current() {
        return Ok(());
    }
    Err(Fault {
        class: FaultClass::Protocol,
        reason: reason::WIRE.to_owned(),
        detail: format!(
            "frame carried wire {observed}; this build speaks {}",
            WireVersion::CURRENT
        ),
    })
}

/// Admit a browser/resident protocol without weakening DTO decoding.
fn admit_app_protocol(observed: Option<AppProtocolVersion>) -> Result<(), Fault> {
    if observed.is_some_and(AppProtocolVersion::is_current) {
        return Ok(());
    }
    Err(Fault {
        class: FaultClass::Protocol,
        reason: reason::APP_PROTOCOL.to_owned(),
        detail: observed.map_or_else(
            || {
                format!(
                    "bootstrap carried no app protocol; this build speaks {}",
                    AppProtocolVersion::CURRENT
                )
            },
            |observed| {
                format!(
                    "bootstrap carried app protocol {observed}; this build speaks {}",
                    AppProtocolVersion::CURRENT
                )
            },
        ),
    })
}

/// One capability-scoped connection to one resident generation.
///
/// A session is the smallest thing that can be stale. Each `spice up` replaces
/// endpoint, capability, and epoch together, so a session never outlives the
/// generation it opened against and never silently adopts a new capability.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    capability: CanonicalUuid,
    epoch: CanonicalUuid,
}

impl fmt::Debug for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Session")
            .field("capability", &"[redacted]")
            .field("epoch", &self.epoch)
            .finish()
    }
}

impl Session {
    /// Open a session against one resident generation.
    #[must_use]
    pub const fn open(capability: CanonicalUuid, epoch: CanonicalUuid) -> Self {
        Self { capability, epoch }
    }

    /// The generation this session addresses.
    #[must_use]
    pub const fn epoch(&self) -> CanonicalUuid {
        self.epoch
    }

    const fn scope(&self) -> SessionScope {
        SessionScope {
            capability: self.capability,
            epoch: self.epoch,
        }
    }

    /// The opening frame, always at the version this build speaks.
    ///
    /// The capability crosses only inside this payload. It never reaches a
    /// URL, query string, fragment, or cookie, so browser history and logs
    /// cannot leak it.
    #[must_use]
    pub const fn hello(&self) -> Hello {
        Hello {
            wire: WireVersion::CURRENT,
            capability: self.capability,
            epoch: self.epoch,
        }
    }

    /// Admit the app protocol advertised by bootstrap discovery.
    ///
    /// # Errors
    ///
    /// Returns a protocol fault when the resident asks this browser build to
    /// cross an app DTO generation it does not speak.
    pub fn admit_app_protocol(observed: Option<AppProtocolVersion>) -> Result<(), Fault> {
        admit_app_protocol(observed)
    }

    /// Decode the resident's acceptance and admit it against this session.
    ///
    /// This is the only way the app learns which principal it acts as. The
    /// resident derives actor and rights from the capability it was presented,
    /// so the app never asserts an actor and has no constructor that would let
    /// it: a principal enters through this door or not at all.
    ///
    /// # Errors
    ///
    /// Returns a protocol fault when the bytes do not decode, carry a version
    /// this build does not speak, or accept a generation this session does not
    /// address.
    pub fn admit_accepted(&self, bytes: &[u8]) -> Result<Admitted<Accepted>, Fault> {
        let frame: Accepted = decode_frame(bytes)?;
        admit_wire(frame.wire)?;
        self.admit_epoch(frame.epoch)?;
        Ok(Admitted {
            frame,
            scope: self.scope(),
        })
    }

    /// Decode one inbound response and admit it against this session.
    ///
    /// Undecodable bytes, a version this build does not speak, and an epoch
    /// that has moved are all protocol faults. They are reported rather than
    /// repaired: there is no upcast path and no second decoder.
    ///
    /// # Errors
    ///
    /// Returns a protocol fault naming which of the three admissions failed.
    pub fn admit_frame<B: DeserializeOwned>(
        &self,
        bytes: &[u8],
    ) -> Result<Admitted<Response<B>>, Fault> {
        let frame: Response<B> = decode_frame(bytes)?;
        admit_wire(frame.v)?;
        self.admit_epoch(frame.epoch)?;
        Ok(Admitted {
            frame,
            scope: self.scope(),
        })
    }

    /// Decode one multiplexed app stream frame and admit every generation
    /// and wire marker it carries.
    ///
    /// The outer frame is not enough for fact pages: their envelopes carry the
    /// current wire independently, and terminal cursors restate the generation
    /// they can resume. This door checks each marker before returning the sole
    /// receipt browser runtime state accepts.
    ///
    /// # Errors
    ///
    /// Returns a protocol fault when the frame is undecodable, any wire marker
    /// is foreign, or the frame or one of its cursors names another generation.
    pub fn admit_stream(&self, bytes: &[u8]) -> Result<Admitted<StreamFrame>, Fault> {
        let frame: StreamFrame = decode_frame(bytes)?;
        let (wire, epoch) = match &frame {
            StreamFrame::Facts {
                v, epoch, packed, ..
            }
            | StreamFrame::Rooms {
                v, epoch, packed, ..
            }
            | StreamFrame::Thread {
                v, epoch, packed, ..
            } => {
                for envelope in &packed.rows {
                    admit_wire(envelope.wire())?;
                }
                (*v, *epoch)
            }
            StreamFrame::Fact { v, epoch, fact, .. } => {
                admit_wire(fact.wire())?;
                (*v, *epoch)
            }
            StreamFrame::Trust { v, epoch, .. }
            | StreamFrame::Messages { v, epoch, .. }
            | StreamFrame::MessageHistory { v, epoch, .. }
            | StreamFrame::MessageGeneration { v, epoch, .. }
            | StreamFrame::Quota { v, epoch, .. }
            | StreamFrame::QuotaGeneration { v, epoch, .. }
            | StreamFrame::Refusal { v, epoch, .. }
            | StreamFrame::Fault { v, epoch, .. }
            | StreamFrame::Pong { v, epoch, .. } => (*v, *epoch),
            StreamFrame::Overflow {
                v, epoch, after, ..
            }
            | StreamFrame::Closing {
                v, epoch, after, ..
            } => {
                self.admit_epoch(after.epoch)?;
                (*v, *epoch)
            }
        };
        admit_wire(wire)?;
        self.admit_epoch(epoch)?;
        Ok(Admitted {
            frame,
            scope: self.scope(),
        })
    }

    /// Admit a generation against the one this session opened on.
    ///
    /// Both epoch-bearing doors share this spelling. A second one would let a
    /// caller handle one door and miss the other.
    fn admit_epoch(&self, observed: CanonicalUuid) -> Result<(), Fault> {
        if observed == self.epoch {
            return Ok(());
        }
        Err(Fault {
            class: FaultClass::Protocol,
            reason: reason::EPOCH.to_owned(),
            detail: format!(
                "frame answered epoch {observed}; this session addresses {}",
                self.epoch
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_protocol_admits_only_the_current_generation() {
        assert!(admit_app_protocol(Some(AppProtocolVersion::CURRENT)).is_ok());

        for observed in [
            None,
            Some(AppProtocolVersion(AppProtocolVersion::CURRENT.0 - 1)),
            Some(AppProtocolVersion(AppProtocolVersion::CURRENT.0 + 1)),
        ] {
            let fault = admit_app_protocol(observed)
                .expect_err("a non-current app protocol must stop before DTO decoding");
            assert_eq!(fault.class, FaultClass::Protocol);
            assert_eq!(fault.reason, reason::APP_PROTOCOL);
        }
    }
}
