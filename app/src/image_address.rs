//! The lightbox's durable image addresses: which retained image a locator
//! names, and the one route that image is addressed over.
//!
//! An image has a durable address only through what retains it: a lane message
//! card or a task. Its locator rides its owner's own route, so the address
//! opens the same image wherever it is followed from; over any other route the
//! locator names nothing.

use crate::view::{AppRoute, ImageTarget, WorkPlace};

/// What retains an addressed image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner<'a> {
    /// A lane message card, by its card key.
    Card(&'a str),
    /// A task, by its birth stamp.
    Task(&'a str),
}

/// How a locator picks one image among those its owner retains.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pick<'a> {
    /// The image's media identity.
    Identity(&'a str),
    /// The image's place among its owner's images, for one with no identity.
    Ordinal(usize),
}

/// One retained image: its owner and its place there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Locator<'a> {
    pub(crate) owner: Owner<'a>,
    pub(crate) pick: Pick<'a>,
}

impl<'a> Locator<'a> {
    /// The image `target` names, if it spells one.
    #[must_use]
    pub(crate) fn parse(target: &'a ImageTarget) -> Option<Self> {
        let mut parts = target.as_str().splitn(4, ':');
        let (kind, owner, pick, value) =
            (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
        Some(Self {
            owner: match kind {
                "c" => Owner::Card(owner),
                "t" => Owner::Task(owner),
                _ => return None,
            },
            pick: match pick {
                "m" => Pick::Identity(value),
                "n" => Pick::Ordinal(value.parse().ok()?),
                _ => return None,
            },
        })
    }

    /// The route state naming this image.
    #[must_use]
    pub(crate) fn target(self) -> Option<ImageTarget> {
        let (kind, owner) = match self.owner {
            Owner::Card(card) => ('c', card),
            Owner::Task(task) => ('t', task),
        };
        ImageTarget::new(match self.pick {
            Pick::Identity(identity) => format!("{kind}:{owner}:m:{identity}"),
            Pick::Ordinal(ordinal) => format!("{kind}:{owner}:n:{ordinal}"),
        })
    }
}

impl Owner<'_> {
    /// The one route this owner's images are addressed over, reached from
    /// `current`: a card's own route, or a task's own address. A task `current`
    /// already opens keeps its view and adjustments, so its images open in
    /// place.
    #[must_use]
    pub(crate) fn route(self, current: &AppRoute) -> Option<AppRoute> {
        match self {
            Self::Card(card) => AppRoute::card(card.to_owned()),
            Self::Task(task) => {
                if current
                    .task()
                    .is_some_and(|(opened, _)| opened.as_str() == task)
                {
                    return Some(current.clone());
                }
                Some(AppRoute::work_place(WorkPlace::Task {
                    task: task.parse().ok()?,
                    view: None,
                }))
            }
        }
    }
}

/// Whether `route` is the route `target`'s image is addressed over.
#[must_use]
pub(crate) fn addressed_over(route: &AppRoute, target: &ImageTarget) -> bool {
    Locator::parse(target)
        .and_then(|locator| locator.owner.route(route))
        .is_some_and(|own| own == *route)
}

#[cfg(test)]
mod tests {
    use super::{Locator, Owner, Pick, addressed_over};
    use crate::view::{AppRoute, ImageTarget, normalize_app_location};

    const CARD: &str = "message-agent-018f82d4-6f59-7d06-92e3-bce913116418-1-0-510-0";

    fn route(fragment: &str) -> AppRoute {
        normalize_app_location("", fragment).route().clone()
    }

    fn target(text: &str) -> ImageTarget {
        ImageTarget::new(text.to_owned()).expect("fixture image target")
    }

    #[test]
    fn an_image_is_addressed_only_over_its_owners_own_route() {
        let card = target(&format!("c:{CARD}:n:1"));
        assert!(addressed_over(
            &route(&format!("#/chat/card/{CARD}")),
            &card
        ));
        assert!(!addressed_over(&route("#/chat"), &card));
        assert!(!addressed_over(
            &route("#/chat/card/message-agent-other"),
            &card
        ));

        let task = target("t:dYKYdz5Dc:m:6430158d");
        assert!(addressed_over(&route("#/work/task/dYKYdz5Dc"), &task));
        assert!(addressed_over(
            &route("#/work/task/dYKYdz5Dc/description?depth=4"),
            &task
        ));
        assert!(!addressed_over(&route("#/work/all"), &task));
        assert!(!addressed_over(&route("#/work/task/dYKfrp7Jy"), &task));
        assert!(!addressed_over(&route("#/chat"), &task));

        assert!(!addressed_over(&route("#/chat"), &target("garbage")));
    }

    #[test]
    fn a_task_image_opens_over_its_tasks_own_address() {
        let owner = Owner::Task("dYKYdz5Dc");
        // From a list or another task the image moves to its own task's base
        // address; a view of its own task keeps that view.
        for elsewhere in ["#/work/all?depth=4", "#/work/task/dYKfrp7Jy", "#/chat"] {
            assert_eq!(
                owner.route(&route(elsewhere)),
                Some(route("#/work/task/dYKYdz5Dc")),
                "{elsewhere}"
            );
        }
        let viewed = route("#/work/task/dYKYdz5Dc/description?depth=4");
        assert_eq!(owner.route(&viewed), Some(viewed));
        assert_eq!(Owner::Task("not a stamp").route(&route("#/chat")), None);
    }

    #[test]
    fn a_locator_round_trips_through_its_route_state() {
        for locator in [
            Locator {
                owner: Owner::Card(CARD),
                pick: Pick::Ordinal(2),
            },
            Locator {
                owner: Owner::Task("dYKYdz5Dc"),
                pick: Pick::Identity("6430158d"),
            },
        ] {
            let written = locator.target().expect("fixture locator");
            assert_eq!(Locator::parse(&written), Some(locator));
        }
        for malformed in ["c:card", "x:owner:n:0", "c:card:n:first", "c:card:z:0"] {
            assert_eq!(Locator::parse(&target(malformed)), None, "{malformed}");
        }
    }
}
