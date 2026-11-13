//! Room packet routes a write's peer messages open.

use wire::rooms::{RoomAnchor, RoomMessaged};
use wire::{AnyFact, CanonicalUuid};

use super::{Refusal, unsound};

struct OpenedPacketRoute {
    directive: CanonicalUuid,
    room: Option<RoomAnchor>,
    from: CanonicalUuid,
    sender: CanonicalUuid,
    locus: wire::body::TranscriptLocus,
}

impl OpenedPacketRoute {
    fn read(fact: &AnyFact) -> Option<Self> {
        if let Some(message) = fact.read::<wire::body::SteerMessaged>() {
            return Some(Self {
                directive: message.directive,
                room: message.room,
                from: message.from,
                sender: message.sender,
                locus: message.locus,
            });
        }
        fact.read::<wire::body::SteerQueued>().map(|message| Self {
            directive: message.directive,
            room: message.room,
            from: message.from,
            sender: message.sender,
            locus: message.locus,
        })
    }
}

/// Each room record of a packet names exactly the directives the same write's
/// peer messages opened into that room, in write order, and each of those
/// messages was sent from the packet's lane by its author at its hunk's tape
/// record. A write publishes at most one packet, so these are its routes.
pub(crate) fn require_packet_routes<'a>(
    facts: impl IntoIterator<Item = &'a AnyFact>,
) -> Result<(), Refusal> {
    let mut messages = Vec::new();
    let mut packets = Vec::new();
    for fact in facts {
        if let Some(message) = OpenedPacketRoute::read(fact) {
            messages.push(message);
        } else if let Some(packet) = fact.read::<RoomMessaged>() {
            packets.push(packet);
        }
    }
    for packet in &packets {
        let opened = messages
            .iter()
            .filter(|message| message.room.as_ref() == Some(&packet.room))
            .collect::<Vec<_>>();
        if !opened
            .iter()
            .map(|message| message.directive)
            .eq(packet.directed.iter().copied())
        {
            return unsound(format!(
                "room packet in {} names directed routes other than those its write opened there",
                packet.room
            ));
        }
        let hunk = &packet.hunk;
        if let Some(message) = opened.into_iter().find(|message| {
            let locus = &message.locus;
            message.from != hunk.head.lane
                || message.sender != packet.author
                || locus.source != Some(hunk.head.source)
                || (locus.member, locus.offset, locus.length, locus.event)
                    != (
                        hunk.locus.member,
                        hunk.locus.offset,
                        hunk.locus.length,
                        hunk.locus.event,
                    )
        }) {
            return unsound(format!(
                "directive {} in room {} was not sent from this packet's lane by its author at its record",
                message.directive, packet.room
            ));
        }
    }
    Ok(())
}
