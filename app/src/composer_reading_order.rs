//! Sequential keyboard focus through member composer strips in reading order.
//!
//! A strip paints canonical member zero at its right edge, beside the team's
//! controls, so its document order runs right to left on screen. Tab and
//! Shift+Tab instead read each strip's member sections left to right, keep
//! every section's own fields in document order, and leave document order in
//! charge everywhere else, so focus crosses one team and then the next as a
//! person reads them.

/// One sequentially focusable element, listed in document order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stop {
    /// The member section holding this stop, or `None` outside every strip.
    pub section: Option<Section>,
}

/// Where a member section sits: its strip's ordinal among the listed strips
/// and the section's left edge on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Section {
    pub strip: usize,
    pub left: f64,
}

/// Every stop index in reading order. A strip is one subtree, so its stops
/// form one contiguous document run; a stable sort on section edges orders
/// sections left to right and keeps each section's stops in document order.
fn reading_order(stops: &[Stop]) -> Vec<usize> {
    let mut order = (0..stops.len()).collect::<Vec<_>>();
    let strip = |index: usize| stops[index].section.map(|section| section.strip);
    let left = |index: &usize| stops[*index].section.map_or(0.0, |section| section.left);
    let mut start = 0;
    while start < stops.len() {
        let Some(current) = strip(start) else {
            start += 1;
            continue;
        };
        let end = (start..stops.len())
            .find(|&index| strip(index) != Some(current))
            .unwrap_or(stops.len());
        order[start..end].sort_by(|a, b| left(a).total_cmp(&left(b)));
        start = end;
    }
    order
}

/// The stop that Tab, or Shift+Tab when `backward`, focuses from `current`
/// when reading order departs from document order there. `None` leaves the
/// step to the browser's own sequential navigation, including at either end
/// of the document.
pub(crate) fn redirect(stops: &[Stop], current: usize, backward: bool) -> Option<usize> {
    let order = reading_order(stops);
    let position = order.iter().position(|&index| index == current)?;
    let target = *order.get(if backward {
        position.checked_sub(1)?
    } else {
        position + 1
    })?;
    let native = if backward {
        current.checked_sub(1)
    } else {
        Some(current + 1)
    };
    (native != Some(target)).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::{Section, Stop, redirect};

    #[test]
    fn tab_reads_member_sections_left_to_right_then_crosses_to_the_next_team() {
        let outside = Stop { section: None };
        let at = |strip, left| Stop {
            section: Some(Section { strip, left }),
        };
        // Document order: toolbar, team one's canonical members 0 (right edge,
        // two fields) and 1 (left edge), its controls, then team two's members
        // 0 and 1 and the next team's toolbar.
        let stops = [
            outside,      // 0 team one toolbar
            at(0, 320.0), // 1 member zero menu
            at(0, 320.0), // 2 member zero draft
            at(0, 0.0),   // 3 member one draft
            outside,      // 4 team one SEND
            at(1, 900.0), // 5 team two member zero draft
            at(1, 700.0), // 6 team two member one draft
            outside,      // 7 after team two
        ];
        let forward = |current| redirect(&stops, current, false);
        let backward = |current| redirect(&stops, current, true);

        // Entering a strip lands on its leftmost member; each member's own
        // fields keep document order; leaving the rightmost member reaches
        // the controls, and the next team starts again at its left.
        assert_eq!(forward(0), Some(3));
        assert_eq!(forward(3), Some(1));
        assert_eq!(forward(1), None);
        assert_eq!(forward(2), Some(4));
        assert_eq!(forward(4), Some(6));
        assert_eq!(forward(6), Some(5));
        assert_eq!(forward(5), Some(7));
        assert_eq!(forward(7), None);

        // Shift+Tab retraces exactly.
        assert_eq!(backward(7), Some(5));
        assert_eq!(backward(5), Some(6));
        assert_eq!(backward(6), Some(4));
        assert_eq!(backward(4), Some(2));
        assert_eq!(backward(2), None);
        assert_eq!(backward(1), Some(3));
        assert_eq!(backward(3), Some(0));
        assert_eq!(backward(0), None);
    }
}
