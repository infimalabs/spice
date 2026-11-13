//! Transcript hunk admission against accounted generations.

use wire::rooms::TranscriptHunk;

use super::{Authority, Refusal, unsound};

/// A hunk must name a nonempty text range of one decoded event inside the
/// accounted extent of its transcript generation.
pub(super) fn require_hunk(
    authority: &impl Authority,
    hunk: &TranscriptHunk,
) -> Result<(), Refusal> {
    let TranscriptHunk { head, locus, text } = hunk;
    let Some(accounted) = authority.transcript(head)? else {
        return unsound(format!(
            "source transcript {} generation {} is not accounted",
            head.key().as_str(),
            head.source
        ));
    };
    if accounted.source != head.source {
        return unsound(format!(
            "source transcript {} generation {} is not accounted",
            head.key().as_str(),
            head.source
        ));
    }
    if !position_at_or_before(head.next, accounted.next) {
        return unsound(format!(
            "source transcript {} boundary {}/{} exceeds accounted boundary {}/{}",
            head.key().as_str(),
            head.next.member,
            head.next.offset,
            accounted.next.member,
            accounted.next.offset,
        ));
    }
    if !locus_within(*locus, head.next) {
        return unsound(format!(
            "source transcript {} locus {}/{}/{} exceeds linked boundary {}/{}",
            head.key().as_str(),
            locus.member,
            locus.offset,
            locus.length,
            head.next.member,
            head.next.offset,
        ));
    }
    if locus.event == u32::MAX || text.start >= text.end {
        return unsound(format!(
            "source transcript {} hunk {}..{} of event {} is not decoded event text",
            head.key().as_str(),
            text.start,
            text.end,
            locus.event,
        ));
    }
    Ok(())
}

fn position_at_or_before(
    position: wire::body::TranscriptPosition,
    boundary: wire::body::TranscriptPosition,
) -> bool {
    position.member < boundary.member
        || (position.member == boundary.member && position.offset <= boundary.offset)
}

fn locus_within(locus: wire::MessageLocus, boundary: wire::body::TranscriptPosition) -> bool {
    if locus.length == 0 {
        return false;
    }
    let Some(end) = locus.offset.checked_add(locus.length) else {
        return false;
    };
    locus.member < boundary.member || (locus.member == boundary.member && end <= boundary.offset)
}
