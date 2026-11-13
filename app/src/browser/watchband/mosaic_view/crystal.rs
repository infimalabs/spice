//! Crystallized image runs in the packed plane: grouping and cell geometry.
//!
//! [`crate::mosaic_crystal`] decides which cards join and how cells divide a
//! box. Here the newest-first plane folds each run into its first card, and a
//! rendered crystal reads its images' natural sizes from the browser's own
//! decode once they load, then writes each cell's box through the CSP-safe
//! custom-property channel.

use super::*;

/// Fold every run of close single-image tool cards into the run's first card.
///
/// `cards` is newest first, as the plane renders it; runs form oldest first
/// so a new capture extends only the newest run. Cards outside a run, and
/// every card a run leaves alone, pass through unchanged.
pub(super) fn crystallize(cards: &[ActivityCard]) -> Vec<ActivityCard> {
    let ascending: Vec<Option<&wire::AgentMessage>> = cards
        .iter()
        .rev()
        .map(|card| match card {
            ActivityCard::Agent { message, .. }
                if crate::mosaic_crystal::crystallizable(message) =>
            {
                Some(message.as_ref())
            }
            ActivityCard::Agent { .. } | ActivityCard::Lifecycle { .. } => None,
        })
        .collect();
    let mut folded = Vec::with_capacity(cards.len());
    let mut start = 0;
    for length in crate::mosaic_crystal::run_lengths(&ascending) {
        let first = &cards[cards.len() - 1 - start];
        let card = match first {
            ActivityCard::Agent {
                accent,
                member,
                quote_agents,
                quote_sources,
                quote_tasks,
                quote_lane,
                message,
                ..
            } if length > 1 => ActivityCard::Agent {
                accent: *accent,
                member: member.clone(),
                quote_agents: Rc::clone(quote_agents),
                quote_sources: Rc::clone(quote_sources),
                quote_tasks: Rc::clone(quote_tasks),
                quote_lane: *quote_lane,
                message: Rc::clone(message),
                companions: (1..length)
                    .map(|offset| match &cards[cards.len() - 1 - start - offset] {
                        ActivityCard::Agent { message, .. } => Rc::clone(message),
                        ActivityCard::Lifecycle { .. } => {
                            unreachable!("only agent cards join an image run")
                        }
                    })
                    .collect(),
            },
            _ => first.clone(),
        };
        folded.push(card);
        start += length;
    }
    folded.reverse();
    folded
}

/// The cell geometry channel: each property reads its cell's fraction.
const CELL_PROPERTIES: [&str; 4] = [
    "--crystal-cell-x",
    "--crystal-cell-y",
    "--crystal-cell-width",
    "--crystal-cell-height",
];

/// Keep a rendered crystal divided for its own box.
///
/// The lattice may give the card any footprint, landscape or portrait, and
/// the plane writes it after the card renders; observing the crystal's box
/// re-chooses the cells on every change of shape, including the first.
#[hook]
pub(in crate::browser::watchband) fn use_crystal_arrangement(
    crystal: NodeRef,
    companions: Rc<[Rc<wire::AgentMessage>]>,
) {
    use_effect_with((crystal, companions), |(crystal, _)| {
        let observed = crystal.cast::<web_sys::Element>().and_then(|element| {
            let target = element.clone();
            let resized = Closure::<dyn FnMut()>::new(move || arrange_crystal(&target));
            let observer = web_sys::ResizeObserver::new(resized.as_ref().unchecked_ref()).ok()?;
            observer.observe(&element);
            Some((observer, resized))
        });
        move || {
            if let Some((observer, _)) = &observed {
                observer.disconnect();
            }
            drop(observed);
        }
    });
}

/// Divide one rendered crystal among its images.
///
/// An image the browser has not decoded yet reads as square; its load event
/// arranges the crystal again with its natural size.
pub(super) fn arrange_crystal(crystal: &web_sys::Element) {
    let Ok(images) = crystal.query_selector_all(".mosaic-crystal-cell img") else {
        return;
    };
    let mut cells = Vec::with_capacity(images.length() as usize);
    let mut aspects = Vec::with_capacity(images.length() as usize);
    for index in 0..images.length() {
        let Some(image) = images
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::HtmlImageElement>().ok())
        else {
            return;
        };
        let Some(cell) = image
            .closest(".mosaic-crystal-cell")
            .ok()
            .flatten()
            .and_then(|cell| cell.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            return;
        };
        aspects.push(f64::from(image.natural_width()) / f64::from(image.natural_height()));
        cells.push(cell);
    }
    let frame = f64::from(crystal.client_width()) / f64::from(crystal.client_height());
    for (cell, geometry) in cells
        .iter()
        .zip(crate::mosaic_crystal::arrange(&aspects, frame))
    {
        let style = cell.style();
        for (property, fraction) in
            CELL_PROPERTIES
                .iter()
                .zip([geometry.x, geometry.y, geometry.width, geometry.height])
        {
            let value = format!("{:.4}%", fraction * 100.0);
            if style
                .get_property_value(property)
                .is_ok_and(|current| current == value)
            {
                continue;
            }
            let _ = style.set_property(property, &value);
        }
    }
}
