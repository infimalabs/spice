//! The one description of the protected group's slot in an owner record.
//!
//! A wrapper cannot know the group it is about to protect: the group does not
//! exist until the child does, and `spawn` returns only once that child has
//! already `exec`ed. Everything between the fork and the record naming the
//! group is therefore a window in which a killed wrapper leaves possession to a
//! guardian nothing on disk names, and no later reader can tell that waiting
//! for it would ever end.
//!
//! The window is closed by writing the record before the fork with the group
//! left blank, and letting the guardian -- which is already alive, already
//! holds the descriptor, and already learns the child's group before `exec` --
//! fill the blank in. Three parties then read one shape: the wrapper reserving
//! the slot, the reader deciding whether a wait is bounded, and the guardian
//! publishing into it. It is described here once rather than three times in
//! agreement.

use serde::de::{Deserializer, Error as _};
use serde::{Deserialize, Serialize, Serializer};

/// How many bytes the group occupies, whatever it currently says.
///
/// Ten digits spans every `u32`, so a published group never needs more and a
/// blank one is never given fewer. Both states being the same width is what
/// lets the guardian publish with a single positioned write, without moving a
/// byte the wrapper may be rewriting at the same moment.
pub(crate) const GROUP_SLOT_BYTES: usize = 10;

/// The byte a slot carries until a group is published into it.
///
/// It is not a digit, and that is the whole reason a torn publication is safe.
/// A positioned write that lands only partly leaves this byte in every position
/// it did not reach, and a slot holding one is not a group -- so no prefix of a
/// publication can be read as some other, smaller, entirely real process group.
pub(crate) const GROUP_SLOT_BLANK: u8 = b'-';

/// What immediately precedes the slot, and what the guardian seeks to find it.
///
/// The leading comma is part of the needle because the slot is never the first
/// field, and because a `cwd` cannot forge it: a quote inside a path is escaped
/// on the way into the record, so the bytes spelled here cannot occur in one.
///
/// Only a guardian goes looking, and only Unix forks one; elsewhere the record
/// is read and written whole, by whoever wrote it.
#[cfg(unix)]
pub(crate) const GROUP_SLOT_PREFIX: &[u8] = br#","protected_group":""#;

/// The protected group as the record spells it, published or still blank.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProtectedGroup {
    /// A group the guardian has written into the slot.
    Published(u32),
    /// A slot reserved before the fork and not yet filled in.
    ///
    /// A torn publication arrives here too, because a reader that cannot name
    /// the group must not act as though it had named a finished one. A slot of
    /// some other width arrives nowhere: nothing here writes one, so it is
    /// refused outright and takes its whole record with it.
    Blank,
}

impl ProtectedGroup {
    /// The group, if this slot names one at all.
    ///
    /// Only a reader deciding whether a wait is bounded asks, and only Unix has
    /// the process groups that question is about.
    #[cfg(unix)]
    pub(crate) const fn group(self) -> Option<u32> {
        match self {
            Self::Published(group) => Some(group),
            Self::Blank => None,
        }
    }
}

impl Serialize for ProtectedGroup {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let slot = match *self {
            Self::Published(group) => group_slot(group),
            Self::Blank => [GROUP_SLOT_BLANK; GROUP_SLOT_BYTES],
        };
        let slot = std::str::from_utf8(&slot).map_err(serde::ser::Error::custom)?;
        serializer.serialize_str(slot)
    }
}

impl<'de> Deserialize<'de> for ProtectedGroup {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let slot = String::deserialize(deserializer)?;
        if slot.len() != GROUP_SLOT_BYTES {
            return Err(D::Error::invalid_length(
                slot.len(),
                &"a ten byte group slot",
            ));
        }
        Ok(slot_group(slot.as_bytes()).map_or(Self::Blank, Self::Published))
    }
}

/// One group as the fixed-width digits a positioned write can place.
///
/// The guardian calls this between a `fork` and its `_exit`, so it allocates
/// nothing and calls nothing: it is arithmetic over a stack array.
pub(crate) const fn group_slot(group: u32) -> [u8; GROUP_SLOT_BYTES] {
    let mut slot = [b'0'; GROUP_SLOT_BYTES];
    let mut remaining = group;
    let mut index = GROUP_SLOT_BYTES;
    while index > 0 {
        index -= 1;
        slot[index] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
    }
    slot
}

/// The group a slot names, or nothing if it names none.
///
/// A slot of the wrong width, one holding any byte that is not a digit, and one
/// spelling zero all answer the same way. Zero is refused because it is the one
/// value a group probe reads as the prober's own group rather than as somebody
/// else's, which would let a blank record be mistaken for a finished one.
pub(crate) fn slot_group(slot: &[u8]) -> Option<u32> {
    if slot.len() != GROUP_SLOT_BYTES || !slot.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut group: u32 = 0;
    for digit in slot {
        group = group
            .checked_mul(10)?
            .checked_add(u32::from(digit - b'0'))?;
    }
    (group != 0).then_some(group)
}

#[cfg(test)]
mod tests {
    use super::{GROUP_SLOT_BLANK, GROUP_SLOT_BYTES, ProtectedGroup, group_slot, slot_group};

    #[test]
    fn every_group_is_spelled_at_the_one_width_the_slot_reserves() {
        for group in [1, 9, 10, 84_085, u32::from(u16::MAX), u32::MAX] {
            let slot = group_slot(group);
            assert_eq!(slot.len(), GROUP_SLOT_BYTES);
            assert_eq!(
                slot_group(&slot),
                Some(group),
                "{group} reads back from {:?}",
                String::from_utf8_lossy(&slot)
            );
        }
    }

    #[test]
    fn no_prefix_of_a_publication_can_be_read_as_a_group() {
        let published = group_slot(84_085);
        // Every torn write is a prefix of the publication over the remains of
        // the blank, and this walks all of them: only the whole of it names a
        // group, and the blank is not a group at any width it survives in.
        for placed in 0..GROUP_SLOT_BYTES {
            let mut torn = [GROUP_SLOT_BLANK; GROUP_SLOT_BYTES];
            torn[..placed].copy_from_slice(&published[..placed]);
            assert_eq!(
                slot_group(&torn),
                None,
                "a slot torn after {placed} bytes named a group: {:?}",
                String::from_utf8_lossy(&torn)
            );
        }
        assert_eq!(slot_group(&published), Some(84_085));
    }

    #[test]
    fn a_slot_that_is_not_ten_digits_names_nothing() {
        assert_eq!(slot_group(b"0000084085"), Some(84_085));
        assert_eq!(slot_group(b"84085"), None, "a narrower slot");
        assert_eq!(slot_group(b"00000840850"), None, "a wider slot");
        assert_eq!(slot_group(b"----------"), None, "the blank slot");
        assert_eq!(slot_group(b"0000000000"), None, "zero is nobody's group");
        assert_eq!(slot_group(b" 000084085"), None, "padding is not a digit");
        assert_eq!(slot_group(b"+000084085"), None, "a sign is not a digit");
    }

    #[test]
    fn a_slot_round_trips_through_the_record_it_is_written_into() {
        let published = serde_json::to_string(&ProtectedGroup::Published(84_085))
            .expect("a published slot serializes");
        assert_eq!(published, r#""0000084085""#);
        let blank = serde_json::to_string(&ProtectedGroup::Blank).expect("a blank slot serializes");
        assert_eq!(blank, r#""----------""#);

        for (slot, expected) in [
            (published.as_str(), ProtectedGroup::Published(84_085)),
            (blank.as_str(), ProtectedGroup::Blank),
            (r#""00000-----""#, ProtectedGroup::Blank),
        ] {
            assert_eq!(
                serde_json::from_str::<ProtectedGroup>(slot).expect("a slot reads"),
                expected
            );
        }
        #[cfg(unix)]
        assert_eq!(ProtectedGroup::Published(84_085).group(), Some(84_085));
        #[cfg(unix)]
        assert_eq!(ProtectedGroup::Blank.group(), None);

        // A slot of another width is refused outright rather than read as
        // blank, because nothing this crate writes can produce one: only a
        // record from some other program could, and it is not one to guess at.
        assert!(serde_json::from_str::<ProtectedGroup>(r#""84085""#).is_err());
    }
}
