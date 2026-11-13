//! Member-local progressive history requests for one merged Mosaic plane.

use std::collections::{HashMap, HashSet};

use super::*;

#[derive(Clone)]
pub(in crate::browser) struct HistorySender(SenderHandle);

impl HistorySender {
    pub(in crate::browser) fn new(sender: SenderHandle) -> Self {
        Self(sender)
    }

    fn request(&self, cursor: wire::MessageHistoryCursor) -> bool {
        admit_action(
            &self.0,
            ClientAction::MessageHistory {
                generation: cursor.generation,
                lane: cursor.lane,
                before: cursor.before,
            },
        )
    }
}

impl PartialEq for HistorySender {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0.queue, &other.0.queue)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct HistoryRequestKey {
    revision: u64,
    cursor: wire::MessageHistoryCursor,
}

#[derive(Clone, PartialEq)]
struct HistoryTarget {
    cursor: wire::MessageHistoryCursor,
    completion: u64,
    prose_missing: bool,
    key: String,
    request_key: HistoryRequestKey,
}

fn history_targets(
    cards: &[ActivityCard],
    targets: &BTreeMap<CanonicalUuid, (wire::MessageHistoryCursor, u64)>,
    revision: u64,
) -> Vec<HistoryTarget> {
    targets
        .iter()
        .map(|(lane, (cursor, completion))| {
            let prose_missing = !cards.iter().any(|card| match card {
                ActivityCard::Agent { message, .. } => {
                    message.lane == *lane
                        && message.activity == wire::AgentActivityKind::Prose
                        && crate::watchband::has_authored_prose(&message.text)
                }
                ActivityCard::Lifecycle { .. } => false,
            });
            let key = cursor
                .before
                .and_then(|before| {
                    cards.iter().find_map(|card| match card {
                        ActivityCard::Agent { message, .. }
                            if message.lane == *lane && message.locus == before =>
                        {
                            Some(card.key())
                        }
                        ActivityCard::Agent { .. } | ActivityCard::Lifecycle { .. } => None,
                    })
                })
                // Cards are newest first; the lane's oldest card borders its history.
                .or_else(|| {
                    cards.iter().rev().find_map(|card| match card {
                        ActivityCard::Agent { message, .. } if message.lane == *lane => {
                            Some(card.key())
                        }
                        ActivityCard::Agent { .. } | ActivityCard::Lifecycle { .. } => None,
                    })
                })
                .unwrap_or_else(|| format!("history-bootstrap-{lane}"));
            HistoryTarget {
                cursor: *cursor,
                completion: *completion,
                prose_missing,
                key,
                request_key: HistoryRequestKey {
                    revision,
                    cursor: *cursor,
                },
            }
        })
        .collect()
}

fn rotation_start(targets: &[HistoryTarget], last: Option<CanonicalUuid>) -> usize {
    last.and_then(|last| {
        targets
            .iter()
            .position(|target| target.cursor.lane == last)
            .map(|index| (index + 1) % targets.len())
    })
    .unwrap_or(0)
}

/// Lanes whose admitted page a reconnect orphaned: the dropped socket can no
/// longer answer it, and the lane has completed no page since. The reader's
/// demand for that page stands, wherever sibling pages have since moved its
/// frontier card.
fn orphaned_lanes(
    requested: &HashMap<HistoryRequestKey, u64>,
    targets: &[HistoryTarget],
) -> Vec<CanonicalUuid> {
    targets
        .iter()
        .filter(|target| {
            requested.iter().any(|(key, completion)| {
                key.cursor.lane == target.cursor.lane
                    && key.revision != target.request_key.revision
                    && *completion == target.completion
            })
        })
        .map(|target| target.cursor.lane)
        .collect()
}

/// Whether a lane's placed frontier card lies within reach of the viewport.
///
/// A card still measuring is hidden at the plane's top edge, so only a placed
/// frontier can show demand for older pages.
fn frontier_is_near(observed: &web_sys::Element, viewport: &web_sys::DomRect, key: &str) -> bool {
    let selector = format!("[data-mosaic-card-key=\"{key}\"][data-layout-phase=\"committed\"]");
    observed
        .query_selector(&selector)
        .ok()
        .flatten()
        .is_some_and(|card| {
            let bounds = card.get_bounding_client_rect();
            bounds.top() <= viewport.bottom() + 400.0 && bounds.bottom() >= viewport.top() - 400.0
        })
}

/// One registration's view of the plane and its member frontiers.
struct HistoryPager {
    observed: web_sys::Element,
    targets: Vec<HistoryTarget>,
    requested: Rc<RefCell<HashMap<HistoryRequestKey, u64>>>,
    orphaned: Rc<RefCell<HashSet<CanonicalUuid>>>,
    request: HistorySender,
    last_admitted: Rc<RefCell<Option<CanonicalUuid>>>,
}

impl HistoryPager {
    /// Admit every frontier the reader currently demands.
    fn inspect(&self, fresh_demand: bool) {
        // Without fresh demand, an already requested frontier stays closed, so a
        // plane whose every frontier is requested reads no geometry at all.
        if !fresh_demand && {
            let requested = self.requested.borrow();
            self.targets
                .iter()
                .all(|target| requested.contains_key(&target.request_key))
        } {
            return;
        }
        // A plane with no rendered viewport has no trailing edge, so it pages
        // nothing until shown.
        if self.observed.client_height() == 0 {
            return;
        }
        let at_edge = crate::mosaic::history_edge_is_near(
            f64::from(self.observed.scroll_top()),
            f64::from(self.observed.client_height()),
            f64::from(self.observed.scroll_height()),
            None,
        );
        let viewport = self.observed.get_bounding_client_rect();
        let start = rotation_start(&self.targets, *self.last_admitted.borrow());
        for offset in 0..self.targets.len() {
            let target = &self.targets[(start + offset) % self.targets.len()];
            let orphaned = self.orphaned.borrow().contains(&target.cursor.lane);
            // Missing composer prose is its own lane-local demand: it can walk
            // successive page frontiers even after those frontiers leave the
            // viewport. The request key still closes each exact frontier after
            // one request; only a real scroll may retry it.
            let frontier_near = frontier_is_near(&self.observed, &viewport, &target.key);
            let requested_completion = self.requested.borrow().get(&target.request_key).copied();
            if !crate::watchband::history_frontier_request_due(
                requested_completion,
                target.completion,
                fresh_demand,
                target.prose_missing,
                at_edge,
                orphaned,
                frontier_near,
            ) {
                continue;
            }
            self.requested
                .borrow_mut()
                .insert(target.request_key, target.completion);
            if self.request.request(target.cursor) {
                *self.last_admitted.borrow_mut() = Some(target.cursor.lane);
                self.orphaned.borrow_mut().remove(&target.cursor.lane);
            } else {
                self.requested.borrow_mut().remove(&target.request_key);
            }
        }
    }
}

#[hook]
pub(super) fn use_history_paging(
    host: NodeRef,
    cards: &[ActivityCard],
    targets: &BTreeMap<CanonicalUuid, (wire::MessageHistoryCursor, u64)>,
    revision: u64,
    commits: u64,
    request: HistorySender,
) {
    let targets = history_targets(cards, targets, revision);
    let requested = use_mut_ref(HashMap::<HistoryRequestKey, u64>::new);
    let orphaned = use_mut_ref(HashSet::<CanonicalUuid>::new);
    let last_admitted = use_mut_ref(|| None::<CanonicalUuid>);
    let current_inspect = use_mut_ref(|| None::<Rc<dyn Fn(bool)>>);
    let registered_inspect = current_inspect.clone();
    use_effect_with((host, targets, request), move |(host, targets, request)| {
        let registration = host.cast::<web_sys::Element>().and_then(|element| {
            let current = targets
                .iter()
                .map(|target| target.request_key)
                .collect::<HashSet<_>>();
            let lost = orphaned_lanes(&requested.borrow(), targets);
            orphaned.borrow_mut().extend(lost);
            orphaned
                .borrow_mut()
                .retain(|lane| targets.iter().any(|target| target.cursor.lane == *lane));
            requested
                .borrow_mut()
                .retain(|key, _| current.contains(key));
            let pager = HistoryPager {
                observed: element.clone(),
                targets: targets.clone(),
                requested: requested.clone(),
                orphaned: orphaned.clone(),
                request: request.clone(),
                last_admitted: last_admitted.clone(),
            };
            let inspect: Rc<dyn Fn(bool)> =
                Rc::new(move |fresh_demand: bool| pager.inspect(fresh_demand));
            inspect(false);
            *registered_inspect.borrow_mut() = Some(inspect.clone());
            let on_scroll = {
                let inspect = Rc::clone(&inspect);
                Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                    // A completed sparse page retries only after fresh viewport demand.
                    inspect(true);
                })
            };
            element
                .add_event_listener_with_callback("scroll", on_scroll.as_ref().unchecked_ref())
                .ok()?;
            // Showing the plane again gives it a viewport without scrolling it.
            let on_resize =
                Closure::<dyn FnMut(js_sys::Array, web_sys::ResizeObserver)>::new(move |_, _| {
                    inspect(false);
                });
            let resized = web_sys::ResizeObserver::new(on_resize.as_ref().unchecked_ref()).ok()?;
            resized.observe(&element);
            Some((element, on_scroll, resized, on_resize))
        });
        move || {
            registered_inspect.borrow_mut().take();
            if let Some((element, on_scroll, resized, _on_resize)) = registration {
                resized.disconnect();
                let _ = element.remove_event_listener_with_callback(
                    "scroll",
                    on_scroll.as_ref().unchecked_ref(),
                );
            }
        }
    });
    // Each plane commit places cards, so it is the moment a frontier can come
    // into range. Re-inspecting the standing registration then needs no timer,
    // no hidden geometry, and no fresh observer.
    use_effect_with(commits, move |_| {
        let inspect = current_inspect.borrow().clone();
        if let Some(inspect) = inspect {
            inspect(false);
        }
    });
}
