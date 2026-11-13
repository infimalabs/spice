//! Browser measurement and Rust-lattice rendering for team activity.

use super::*;

use yew::functional::UseReducerDispatcher;

const MIN_MEASURABLE_WIDTH_PX: i32 = 24;
mod composer_collapse;
mod crystal;
use crystal::arrange_crystal;
pub(super) use crystal::use_crystal_arrangement;
mod history;
pub(in crate::browser) use history::HistorySender;
use history::use_history_paging;

mod image_resolution;
pub(crate) use image_resolution::use_markdown_image_resolution_events;
use image_resolution::{use_element_height, use_element_width, use_image_resolution_events};

#[derive(Clone)]
pub(in crate::browser) enum ActivityCard {
    Agent {
        accent: usize,
        member: crate::runtime::LaneMember,
        quote_agents: Rc<crate::composer_markdown::QuoteAgents>,
        quote_sources: Rc<crate::composer_markdown::QuoteSources>,
        quote_tasks: Rc<crate::runtime::QuoteTasks>,
        quote_lane: CanonicalUuid,
        message: Rc<wire::AgentMessage>,
        /// Later images crystallized into this card, oldest first; empty for
        /// every card the mosaic shows on its own.
        companions: Rc<[Rc<wire::AgentMessage>]>,
    },
    Lifecycle {
        accent: usize,
        member: crate::runtime::LaneMember,
        boundary: crate::runtime::LaneLifecycleBoundary,
    },
}

impl PartialEq for ActivityCard {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Agent {
                    accent: left_accent,
                    member: left_member,
                    quote_agents: left_quote_agents,
                    quote_sources: left_quote_sources,
                    quote_tasks: left_quote_tasks,
                    quote_lane: left_quote_lane,
                    message: left_message,
                    companions: left_companions,
                },
                Self::Agent {
                    accent: right_accent,
                    member: right_member,
                    quote_agents: right_quote_agents,
                    quote_sources: right_quote_sources,
                    quote_tasks: right_quote_tasks,
                    quote_lane: right_quote_lane,
                    message: right_message,
                    companions: right_companions,
                },
            ) => {
                left_accent == right_accent
                    && left_member.lane == right_member.lane
                    && left_member.name == right_member.name
                    && left_member.worktree == right_member.worktree
                    && left_quote_agents == right_quote_agents
                    && left_quote_sources == right_quote_sources
                    && left_quote_tasks == right_quote_tasks
                    && left_quote_lane == right_quote_lane
                    && (Rc::ptr_eq(left_message, right_message) || left_message == right_message)
                    && left_companions == right_companions
            }
            (
                Self::Lifecycle {
                    accent: left_accent,
                    member: left_member,
                    boundary: left_boundary,
                },
                Self::Lifecycle {
                    accent: right_accent,
                    member: right_member,
                    boundary: right_boundary,
                },
            ) => {
                left_accent == right_accent
                    && left_member == right_member
                    && left_boundary == right_boundary
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug)]
enum ActivityFingerprint {
    Agent {
        worktree: String,
        quote_agents: Rc<crate::composer_markdown::QuoteAgents>,
        quote_sources: Rc<crate::composer_markdown::QuoteSources>,
        quote_tasks: Rc<crate::runtime::QuoteTasks>,
        message: Rc<wire::AgentMessage>,
    },
    Lifecycle(crate::runtime::LaneLifecycleBoundary),
}

impl PartialEq for ActivityFingerprint {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Agent {
                    worktree: left_worktree,
                    quote_agents: left_quote_agents,
                    quote_sources: left_quote_sources,
                    quote_tasks: left_quote_tasks,
                    message: left_message,
                },
                Self::Agent {
                    worktree: right_worktree,
                    quote_agents: right_quote_agents,
                    quote_sources: right_quote_sources,
                    quote_tasks: right_quote_tasks,
                    message: right_message,
                },
            ) => {
                left_worktree == right_worktree
                    && left_quote_agents == right_quote_agents
                    && left_quote_sources == right_quote_sources
                    && left_quote_tasks == right_quote_tasks
                    && (Rc::ptr_eq(left_message, right_message)
                        || same_measured_agent_content(left_message, right_message))
            }
            (Self::Lifecycle(left), Self::Lifecycle(right)) => left == right,
            _ => false,
        }
    }
}

/// Receipt settlement changes only a fixed-width inline pill. Treating it as
/// new measured content would transiently rewrite the live card's measurement
/// attributes and could move its packed-plane placement.
fn same_measured_agent_content(left: &wire::AgentMessage, right: &wire::AgentMessage) -> bool {
    if left.lane != right.lane
        || left.author != right.author
        || left.locus != right.locus
        || left.authored != right.authored
        || left.authored_synthetic != right.authored_synthetic
        || left.observed != right.observed
        || left.activity != right.activity
        || left.text.trim().is_empty() != right.text.trim().is_empty()
        || left.media != right.media
        || left.media_quotes != right.media_quotes
        || !left
            .directive_contexts
            .iter()
            .map(|context| &context.media_quotes)
            .eq(right
                .directive_contexts
                .iter()
                .map(|context| &context.media_quotes))
        || left.markdown_images != right.markdown_images
    {
        return false;
    }
    let left = crate::composer_markdown::render_received_with_quotes(
        &left.text,
        &left.directive_contexts,
        &left.quotes,
        &crate::composer_markdown::QuoteAgents::new(),
        &BTreeMap::new(),
    );
    let right = crate::composer_markdown::render_received_with_quotes(
        &right.text,
        &right.directive_contexts,
        &right.quotes,
        &crate::composer_markdown::QuoteAgents::new(),
        &BTreeMap::new(),
    );
    crate::composer_markdown::same_measured_shape(&left, &right)
}

/// Route target prefix of every lane lifecycle boundary card.
pub(in crate::browser) const LIFECYCLE_CARD_PREFIX: &str = "lane-lifecycle-";

impl ActivityCard {
    fn key(&self) -> String {
        match self {
            Self::Agent { message, .. } => agent_message_source_id(message),
            Self::Lifecycle { boundary, .. } => {
                format!("{LIFECYCLE_CARD_PREFIX}{}", boundary.seq())
            }
        }
    }

    /// Total display chronology for one team. Callers reverse this ascending
    /// key once to obtain the newest-first plane and narration sequence.
    ///
    /// Equal-time ties break on the canonical numeric locus, never on the
    /// rendered identity. A greater source offset is therefore deterministically
    /// newer within one display millisecond. Older history merged after a live
    /// cursor remains behind that cursor and is never narrated as new activity.
    pub(in crate::browser) fn chronology_key(
        &self,
    ) -> (i64, u8, Option<CanonicalUuid>, u64, u64, u64, u32) {
        match self {
            Self::Agent { message, .. } => {
                let (millis, lane, member, offset, length, event) = message.chronology_key();
                (millis, 0, Some(lane), member, offset, length, event)
            }
            Self::Lifecycle { boundary, .. } => (
                boundary.observed().get().as_millisecond(),
                1,
                Some(boundary.lane()),
                0,
                0,
                boundary.seq().get(),
                0,
            ),
        }
    }

    fn measurement_fingerprint(&self) -> ActivityFingerprint {
        match self {
            Self::Agent {
                member,
                quote_agents,
                quote_sources,
                quote_tasks,
                message,
                ..
            } => ActivityFingerprint::Agent {
                worktree: member.worktree.clone(),
                quote_agents: Rc::clone(quote_agents),
                quote_sources: Rc::clone(quote_sources),
                quote_tasks: Rc::clone(quote_tasks),
                message: Rc::clone(message),
            },
            Self::Lifecycle { boundary, .. } => ActivityFingerprint::Lifecycle(boundary.clone()),
        }
    }

    fn base_span(&self, geometry: &crate::mosaic::Geometry) -> usize {
        if self.is_boundary() {
            crate::mosaic::TRACK_COUNT
        } else {
            geometry.base_span
        }
    }

    fn is_boundary(&self) -> bool {
        match self {
            Self::Agent { message, .. } => message.activity.is_boundary(),
            Self::Lifecycle { .. } => true,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ActivityMeasurement {
    width: i32,
    key: String,
    fingerprint: ActivityFingerprint,
    candidates: Vec<crate::mosaic::Candidate>,
    latched: Option<crate::mosaic::Candidate>,
}

#[derive(Clone, PartialEq)]
pub(super) struct CardLayout {
    pub(super) committed: bool,
    pub(super) track: usize,
    pub(super) span: usize,
    pub(super) base: i32,
    pub(super) row: i32,
    pub(super) rows: i32,
    pub(super) lifecycle: crate::mosaic::Lifecycle,
    pub(super) measurement_index: usize,
    pub(super) measure_span: usize,
}

impl CardLayout {
    fn measuring(measurement_index: usize, base_span: usize) -> Self {
        Self {
            committed: false,
            track: 0,
            span: base_span,
            base: 0,
            row: 1,
            rows: 2,
            lifecycle: crate::mosaic::Lifecycle::Liquid,
            measurement_index,
            measure_span: base_span,
        }
    }

    fn placed(
        measurement_index: usize,
        card: &crate::mosaic::Card,
        measurement: &ActivityMeasurement,
    ) -> Self {
        Self {
            committed: true,
            track: card.track,
            span: card.span,
            base: card.base,
            row: card.base,
            rows: card.rows,
            lifecycle: card.lifecycle,
            measurement_index,
            measure_span: measurement.candidates[0].span,
        }
    }
}

const PLANE_LAYOUT_PROPERTIES: [(&str, &str); 2] = [
    ("--mosaic-max-row", "data-max-row"),
    ("--mosaic-plane-rows", "data-plane-rows"),
];
const CARD_LAYOUT_PROPERTIES: [(&str, &str); 5] = [
    ("--mosaic-base", "data-base"),
    ("--mosaic-rows", "data-rows"),
    ("--mosaic-track", "data-track"),
    ("--mosaic-span", "data-span"),
    ("--mosaic-measure-span", "data-measure-span"),
];

fn copy_layout_property(element: &web_sys::HtmlElement, property: &str, attribute: &str) {
    let value = element
        .get_attribute(attribute)
        .unwrap_or_else(|| panic!("Mosaic layout element carries {attribute}"));
    value
        .parse::<i32>()
        .unwrap_or_else(|_| panic!("Mosaic layout attribute {attribute} is an integer: {value}"));
    let style = element.style();
    if style
        .get_property_value(property)
        .is_ok_and(|current| current == value)
    {
        return;
    }
    style
        .set_property(property, &value)
        .unwrap_or_else(|_| panic!("Mosaic layout writes {property}"));
}

fn copy_layout_properties(element: &web_sys::HtmlElement, properties: &[(&str, &str)]) {
    for (property, attribute) in properties {
        copy_layout_property(element, property, attribute);
    }
}

#[hook]
/// Copy admitted integer diagnostics into the one CSP-safe layout channel.
///
/// Firefox does not compute the typed `attr()` arithmetic this lattice once
/// used, and the app CSP deliberately refuses serialized style attributes.
/// `CssStyleDeclaration` keeps the diagnostic attributes visible while every
/// browser executes the same custom-property geometry.
fn use_layout_properties(
    host: NodeRef,
    width: i32,
    keys: Vec<String>,
    plane: Option<crate::mosaic_plane::PersistentLanePlane<String>>,
) {
    use_effect_with((host, width, keys, plane), move |(host, width, _, _)| {
        if *width < MIN_MEASURABLE_WIDTH_PX {
            return;
        }
        let host = host
            .cast::<web_sys::HtmlElement>()
            .expect("a rendered Mosaic plane is an HTML element");
        copy_layout_properties(&host, &PLANE_LAYOUT_PROPERTIES);
        let cards = host
            .query_selector_all(".mosaic-message-card")
            .expect("the fixed Mosaic card selector is valid");
        for index in 0..cards.length() {
            let card = cards
                .item(index)
                .expect("the Mosaic card list retains every indexed node")
                .dyn_into::<web_sys::HtmlElement>()
                .expect("a Mosaic card is an HTML element");
            copy_layout_properties(&card, &CARD_LAYOUT_PROPERTIES);
        }
    });
}

struct MeasurementIndex<'a> {
    by_key: BTreeMap<&'a str, Vec<&'a ActivityMeasurement>>,
}

impl<'a> MeasurementIndex<'a> {
    fn new(measurements: &'a [ActivityMeasurement]) -> Self {
        let mut by_key = BTreeMap::<&str, Vec<&ActivityMeasurement>>::new();
        for measurement in measurements {
            by_key
                .entry(measurement.key.as_str())
                .or_default()
                .push(measurement);
        }
        Self { by_key }
    }

    fn get(
        &self,
        key: &str,
        width: i32,
        activity: &ActivityCard,
    ) -> Option<&'a ActivityMeasurement> {
        let fingerprint = activity.measurement_fingerprint();
        self.by_key.get(key)?.iter().copied().find(|measurement| {
            measurement.width == width && measurement.fingerprint == fingerprint
        })
    }

    /// Most recent geometry for this durable card, even when its rendered
    /// fingerprint has changed and a replacement measurement is pending.
    ///
    /// Keeping that admitted geometry on the live node prevents metadata-only
    /// updates from hiding or moving the settled card while an offscreen clone
    /// measures the revision.
    fn latest(&self, key: &str) -> Option<&'a ActivityMeasurement> {
        self.by_key.get(key)?.last().copied()
    }
}

fn activity_measurement<'a>(
    index: &MeasurementIndex<'a>,
    width: i32,
    activity: &ActivityCard,
) -> Option<&'a ActivityMeasurement> {
    let key = activity.key();
    index.get(&key, width, activity)
}

/// Whose activity one packed plane holds.
#[derive(Clone, PartialEq)]
pub(in crate::browser) enum PlaneOwner {
    /// A team's stable identity and runtime work switches. Only a team narrates.
    Team {
        lane: CanonicalUuid,
        drive: bool,
        drain: bool,
    },
}

#[derive(Properties, PartialEq)]
pub(in crate::browser) struct MosaicPlaneProps {
    pub(in crate::browser) owner: PlaneOwner,
    /// Number of team members.
    pub(in crate::browser) members: usize,
    pub(in crate::browser) cards: Vec<ActivityCard>,
    pub(in crate::browser) quote_dispatcher: UseReducerDispatcher<ComposerDrafts>,
    pub(in crate::browser) open_message_task: Callback<(wire::TaskStamp, web_sys::Element)>,
    pub(in crate::browser) speak: Callback<SpeechRequest>,
    pub(in crate::browser) narrate: Callback<NarrationBatch>,
    pub(in crate::browser) speech: crate::speech_queue::SpeechMode,
    pub(in crate::browser) speech_owner: Option<SpeechOwner>,
    pub(in crate::browser) history_targets:
        BTreeMap<CanonicalUuid, (wire::MessageHistoryCursor, u64)>,
    pub(in crate::browser) history_revision: u64,
    pub(in crate::browser) history: HistorySender,
    /// The surface's composer host, carried in this plane's scroll content.
    pub(in crate::browser) composer_slot: web_sys::HtmlElement,
}

struct MosaicPlaneView<'a> {
    properties: &'a MosaicPlaneProps,
    /// The cards the plane places, with each close image run crystallized.
    cards: &'a [ActivityCard],
    measurement_width: i32,
    measurements: &'a [ActivityMeasurement],
    plane: Option<&'a crate::mosaic_plane::PersistentLanePlane<String>>,
    geometry: crate::mosaic::Geometry,
    max_row: Option<i32>,
    narration_cursor: String,
}

impl MosaicPlaneView<'_> {
    fn render(self, host: NodeRef, track_host: NodeRef) -> Html {
        let rendered_cards = render_activity_cards(
            self.properties,
            self.cards,
            self.measurements,
            &self.geometry,
            self.plane,
        );
        let measurement_phase = measurement_phase(self.plane.is_some(), self.measurement_width);
        let plane_rows = self.plane.map_or(1, |plane| plane.rows());
        // Key each activity once against the placed set so a render stays
        // linear in retained cards.
        let pending = self.plane.map_or(self.cards.len(), |plane| {
            let placed = plane
                .cards()
                .iter()
                .map(|entry| entry.key.as_str())
                .collect::<std::collections::HashSet<_>>();
            self.cards
                .iter()
                .filter(|activity| !placed.contains(activity.key().as_str()))
                .count()
        });
        let commits = self.plane.map_or(0, |plane| plane.commits());
        let transition = self
            .plane
            .map_or("deferred", |plane| plane.transition().as_str());
        let committed_width = self.plane.map_or(0, |plane| plane.geometry().width());
        let committed_viewport_rows = self
            .plane
            .map_or(0, |plane| plane.geometry().viewport_rows());
        let members = self.properties.members;
        let (team_plane, drive, drain, label) = match &self.properties.owner {
            PlaneOwner::Team { lane, drive, drain } => (
                Some(lane.to_string()),
                *drive,
                *drain,
                format!(
                    "Packed team plane: drive {}, drain {}, {members} {}",
                    if *drive { "on" } else { "off" },
                    if *drain { "on" } else { "off" },
                    if members == 1 { "member" } else { "members" },
                ),
            ),
        };
        html! {
            <section
                ref={host}
                class="lane-mosaic-plane"
                data-agent-plane={team_plane}
                data-team-drive={drive.to_string()}
                data-team-drain={drain.to_string()}
                data-team-members={members.to_string()}
                data-lanes={self.geometry.lane_count.to_string()}
                data-plane-rows={plane_rows.to_string()}
                data-max-row={self.max_row.unwrap_or_default().to_string()}
                data-module={self.geometry.module.to_string()}
                data-track-count={crate::mosaic::TRACK_COUNT.to_string()}
                data-base-span={self.geometry.base_span.to_string()}
                data-min-lane={self.geometry.min_lane.to_string()}
                data-min-card-rows={crate::mosaic::MIN_CARD_ROWS.to_string()}
                data-tall-threshold={crate::mosaic::TALL_THRESHOLD_PX.to_string()}
                data-measurement-phase={measurement_phase}
                data-measurement-pending={pending.to_string()}
                data-plane-commits={commits.to_string()}
                data-plane-transition={transition}
                data-observed-width={self.measurement_width.to_string()}
                data-committed-width={committed_width.to_string()}
                data-viewport-rows={committed_viewport_rows.to_string()}
                data-narration-cursor={self.narration_cursor}
                tabindex="0"
                aria-label={label}
            >
                {Html::VRef(self.properties.composer_slot.clone().into())}
                <div ref={track_host} class="lane-mosaic-track">
                    {rendered_cards}
                </div>
            </section>
        }
    }
}

/// Where a plane stands between admission and its first committed lattice.
///
/// `base` is the readable fallback for a host that cannot be measured yet: its
/// cards stay in ordinary flow. A measurable host is `measuring` until its
/// lattice commits, and its cards stay hidden meanwhile, so the first frame a
/// reader sees is the committed layout rather than a flow that then reflows.
fn measurement_phase(committed: bool, width: i32) -> &'static str {
    if committed {
        "ready"
    } else if width >= MIN_MEASURABLE_WIDTH_PX {
        "measuring"
    } else {
        "base"
    }
}

fn committed_spans(
    plane: Option<&crate::mosaic_plane::PersistentLanePlane<String>>,
) -> Vec<(String, usize)> {
    plane
        .into_iter()
        .flat_map(crate::mosaic_plane::PersistentLanePlane::cards)
        .map(|entry| (entry.key.clone(), entry.card.span))
        .collect()
}

/// One component per team lane is deliberate: every lane owns one width,
/// candidate stream, engine call, and row floor for all of its worktree
/// members. Sibling teams can never carry a skyline into one another.
#[function_component(MosaicPlane)]
pub(in crate::browser) fn mosaic_plane(properties: &MosaicPlaneProps) -> Html {
    let host = use_node_ref();
    let track_host = use_node_ref();
    let width = use_element_width(track_host.clone());
    let viewport_height = use_element_height(host.clone());
    let measurement_width = width.round() as i32;
    let live_geometry = crate::mosaic::Geometry::new(16.0, f64::from(measurement_width));
    let plane_geometry = (*viewport_height > 0.0).then(|| {
        crate::mosaic_plane::PlaneGeometry::new(
            measurement_width,
            crate::mosaic::viewport_rows_for(
                *viewport_height,
                live_geometry.gap,
                live_geometry.module,
            ),
        )
    });
    let committed = use_state_eq(|| None::<crate::mosaic_plane::PersistentLanePlane<String>>);
    let rendered_plane = (*committed).clone();
    // Close image runs crystallize only here, where cards are placed; narration
    // and every other surface keep one card per image.
    let cards = use_memo(properties.cards.clone(), |cards| {
        crystal::crystallize(cards)
    });
    // Edges exist only in a settled layout: a plane measuring its first lattice
    // shows a placeholder extent that reads as a trailing edge, so it offers no
    // paging targets until the lattice commits and the edge means something.
    // An empty plane still pages: each member it holds misses prose, which is
    // demand of its own, so its newest history arrives from no card at all.
    let unsettled = BTreeMap::new();
    let paging_targets =
        if measurement_phase(rendered_plane.is_some(), measurement_width) == "measuring" {
            &unsettled
        } else {
            &properties.history_targets
        };
    use_history_paging(
        host.clone(),
        &cards,
        paging_targets,
        properties.history_revision,
        rendered_plane
            .as_ref()
            .map_or(0, crate::mosaic_plane::PersistentLanePlane::commits),
        properties.history.clone(),
    );
    let card_keys = cards.iter().map(ActivityCard::key).collect::<Vec<_>>();
    use_layout_properties(
        host.clone(),
        measurement_width,
        card_keys,
        rendered_plane.clone(),
    );
    let committed_spans = committed_spans(rendered_plane.as_ref());
    let measurements = use_state_eq(Vec::<ActivityMeasurement>::new);
    let narration_cursor = use_state_eq(|| None::<String>);
    // Composer images in the slot are the composer's own; only cards resolve here.
    use_image_resolution_events(track_host.clone(), (*cards).clone());
    use_activity_measurements(
        host.clone(),
        measurement_width,
        (*cards).clone(),
        committed_spans,
        measurements.clone(),
    );
    use_persistent_plane(
        committed.clone(),
        plane_geometry,
        (*cards).clone(),
        (*measurements).clone(),
    );
    use_narration(
        &properties.owner,
        properties.cards.clone(),
        properties.speech,
        properties.narrate.clone(),
        narration_cursor.clone(),
    );
    let committed_plane = rendered_plane.as_ref();
    let geometry_width =
        committed_plane.map_or(measurement_width, |plane| plane.geometry().width());
    let geometry = crate::mosaic::Geometry::new(16.0, f64::from(geometry_width));
    let max_row = committed_plane.map(crate::mosaic_plane::PersistentLanePlane::max_row);
    let composer =
        composer_collapse::use_composer_collapse(host.clone(), properties.composer_slot.clone());
    use_plane_scroll(host.clone(), rendered_plane.clone(), composer);
    MosaicPlaneView {
        properties,
        cards: &cards,
        measurement_width,
        measurements: &measurements,
        plane: committed_plane,
        geometry,
        max_row,
        narration_cursor: (*narration_cursor).clone().unwrap_or_default(),
    }
    .render(host, track_host)
}

#[hook]
/// Preserve the viewport only after live measurement produces a ready plane.
///
/// A temporary `None` during incremental measurement deliberately retains the
/// prior ready-plane state, so the eventual row delta has one authoritative
/// baseline and one scroll owner.
fn use_plane_scroll(
    host: NodeRef,
    plane: Option<crate::mosaic_plane::PersistentLanePlane<String>>,
    composer: composer_collapse::ComposerCollapse,
) {
    let anchor = use_mut_ref(|| None::<ScrollAnchor>);
    {
        let anchor = anchor.clone();
        use_effect_with(host.clone(), move |host| {
            let registration = host.cast::<web_sys::Element>().and_then(|scroller| {
                let live_scroller = scroller.clone();
                let live_anchor = anchor.clone();
                let listener = Closure::<dyn FnMut(web_sys::Event)>::new(move |_| {
                    *live_anchor.borrow_mut() = current_scroll_anchor(&live_scroller);
                });
                if scroller
                    .add_event_listener_with_callback("scroll", listener.as_ref().unchecked_ref())
                    .is_err()
                    || scroller
                        .add_event_listener_with_callback_and_bool(
                            "toggle",
                            listener.as_ref().unchecked_ref(),
                            true,
                        )
                        .is_err()
                {
                    let _ = scroller.remove_event_listener_with_callback(
                        "scroll",
                        listener.as_ref().unchecked_ref(),
                    );
                    let _ = scroller.remove_event_listener_with_callback_and_bool(
                        "toggle",
                        listener.as_ref().unchecked_ref(),
                        true,
                    );
                    return None;
                }
                *anchor.borrow_mut() = current_scroll_anchor(&scroller);
                Some((scroller, listener))
            });
            move || {
                if let Some((scroller, listener)) = registration {
                    let _ = scroller.remove_event_listener_with_callback(
                        "scroll",
                        listener.as_ref().unchecked_ref(),
                    );
                    let _ = scroller.remove_event_listener_with_callback_and_bool(
                        "toggle",
                        listener.as_ref().unchecked_ref(),
                        true,
                    );
                }
            }
        });
    }
    use_effect_with((host, plane), move |(host, plane)| {
        let Some(scroller) = host.cast::<web_sys::Element>() else {
            return;
        };
        if plane.is_none()
            || (open_card_menu_anchor(&scroller).is_none()
                && !crate::mosaic::pane_is_scrolled(f64::from(scroller.scroll_top()), None))
        {
            *anchor.borrow_mut() = None;
            return;
        }
        if let Some(previous) = anchor.borrow().clone()
            && let Some(current_offset) = scroll_anchor_offset(&scroller, &previous.key)
        {
            let delta = current_offset - previous.offset;
            if delta.abs() >= 0.5 {
                // The correction keeps cards visually still, so the composer
                // fused to them must not move either.
                composer.correct_scroll(
                    &scroller,
                    (f64::from(scroller.scroll_top()) + delta).round() as i32,
                );
            }
        }
        *anchor.borrow_mut() = current_scroll_anchor(&scroller);
    });
}

#[derive(Clone, Debug, PartialEq)]
struct ScrollAnchor {
    key: String,
    offset: f64,
}

/// The card whose action drawer currently owns the operator's focus. Unlike a
/// normal viewport anchor, this exists at the live edge too: new work may
/// arrive, but it must not move an open action out from under the pointer.
fn open_card_menu_anchor(scroller: &web_sys::Element) -> Option<ScrollAnchor> {
    let menu = scroller
        .query_selector("details.message-actions-drawer[open]")
        .ok()??;
    let card = menu
        .closest(".mosaic-message-card[data-layout-phase=\"committed\"]")
        .ok()??;
    scroll_anchor_for_card(scroller, &card)
}

fn current_scroll_anchor(scroller: &web_sys::Element) -> Option<ScrollAnchor> {
    open_card_menu_anchor(scroller).or_else(|| visible_scroll_anchor(scroller))
}

fn scroll_anchor_for_card(
    scroller: &web_sys::Element,
    card: &web_sys::Element,
) -> Option<ScrollAnchor> {
    let key = card.get_attribute("data-mosaic-card-key")?;
    Some(ScrollAnchor {
        key,
        offset: card.get_bounding_client_rect().top() - scroller.get_bounding_client_rect().top(),
    })
}

fn committed_scroll_cards(scroller: &web_sys::Element) -> Vec<web_sys::Element> {
    let cards = scroller
        .query_selector_all(".mosaic-message-card[data-layout-phase=\"committed\"]")
        .expect("the fixed committed Mosaic selector is valid");
    (0..cards.length())
        .filter_map(|index| cards.item(index))
        .filter_map(|card| card.dyn_into::<web_sys::Element>().ok())
        .collect()
}

fn visible_scroll_anchor(scroller: &web_sys::Element) -> Option<ScrollAnchor> {
    if !crate::mosaic::pane_is_scrolled(f64::from(scroller.scroll_top()), None) {
        return None;
    }
    let viewport = scroller.get_bounding_client_rect();
    committed_scroll_cards(scroller)
        .into_iter()
        .filter_map(|card| {
            let rect = card.get_bounding_client_rect();
            (rect.bottom() > viewport.top() && rect.top() < viewport.bottom()).then(|| {
                let offset = rect.top() - viewport.top();
                let distance = offset.abs();
                let key = card
                    .get_attribute("data-mosaic-card-key")
                    .expect("a committed Mosaic card has a stable key");
                (distance, ScrollAnchor { key, offset })
            })
        })
        .min_by(|left, right| left.0.total_cmp(&right.0))
        .map(|(_, anchor)| anchor)
}

fn scroll_anchor_offset(scroller: &web_sys::Element, key: &str) -> Option<f64> {
    let viewport_top = scroller.get_bounding_client_rect().top();
    committed_scroll_cards(scroller)
        .into_iter()
        .find(|card| card.get_attribute("data-mosaic-card-key").as_deref() == Some(key))
        .map(|card| card.get_bounding_client_rect().top() - viewport_top)
}

#[hook]
fn use_narration(
    owner: &PlaneOwner,
    cards: Vec<ActivityCard>,
    speech: crate::speech_queue::SpeechMode,
    narrate: Callback<NarrationBatch>,
    narration_cursor: UseStateHandle<Option<String>>,
) {
    let materialized = use_mut_ref(|| false);
    let messages = cards
        .iter()
        .filter_map(|card| match card {
            ActivityCard::Agent {
                member, message, ..
            } => Some((member, message)),
            ActivityCard::Lifecycle { .. } => None,
        })
        .filter_map(|(member, message)| {
            let automatic_speech = message
                .automatic_speech
                .as_ref()
                .and_then(|speech| crate::narration::prepared_utterance(&speech.text));
            // A hunk the resident classified as an acknowledgement is a direct
            // response to steering: Narrate voices every word of it, as Speak
            // does, rather than its edges.
            let narration = match message.automatic_speech.as_ref().map(|speech| speech.kind) {
                Some(wire::AutomaticSpeechKind::Acknowledgement) => automatic_speech.clone(),
                Some(wire::AutomaticSpeechKind::Final) | None => {
                    crate::narration::narration_utterance(message.activity, &message.text)
                }
            };
            (narration.is_some() || automatic_speech.is_some()).then(|| NarrationMessage {
                lane: message.lane.to_string(),
                worktree: member.worktree.clone(),
                playback: member.speech.clone(),
                key: agent_message_source_id(message),
                narration,
                automatic_speech,
            })
        })
        .collect::<Vec<_>>();
    let team = match owner {
        PlaneOwner::Team { lane, .. } => *lane,
    };
    use_effect_with((speech, messages), move |(speech, messages)| {
        let first = !*materialized.borrow();
        *materialized.borrow_mut() = true;
        narrate.emit(NarrationBatch {
            team: team.to_string(),
            materialization: first,
            mode: *speech,
            messages: messages.clone(),
        });
        narration_cursor.set(messages.first().map(|message| message.key.clone()));
    });
}

#[hook]
fn use_activity_measurements(
    host: NodeRef,
    width: i32,
    cards: Vec<ActivityCard>,
    committed_spans: Vec<(String, usize)>,
    measurements: UseStateHandle<Vec<ActivityMeasurement>>,
) {
    {
        let measurements = measurements.clone();
        let prior = (*measurements).clone();
        use_effect_with(
            (width, cards, committed_spans),
            move |(width, cards, committed_spans)| {
                let canceled = Rc::new(Cell::new(false));
                let measurement_index = MeasurementIndex::new(&prior);
                let committed_spans = committed_spans
                    .iter()
                    .map(|(key, span)| (key.as_str(), *span))
                    .collect::<BTreeMap<_, _>>();
                let unknown = cards
                    .iter()
                    .enumerate()
                    .filter_map(|(index, activity)| {
                        let key = activity.key();
                        activity_measurement(&measurement_index, *width, activity)
                            .is_none()
                            .then(|| {
                                let latched_span = committed_spans.get(key.as_str()).copied();
                                (index, activity.clone(), latched_span)
                            })
                    })
                    .collect::<Vec<_>>();
                if *width >= MIN_MEASURABLE_WIDTH_PX && !unknown.is_empty() {
                    let canceled_task = canceled.clone();
                    let host = host.clone();
                    let measurements = measurements.clone();
                    let prior = prior.clone();
                    let width = *width;
                    spawn_local(async move {
                        let measured =
                            measure_unknown(host, width, unknown, canceled_task.clone()).await;
                        if let Some(measured) = measured
                            && !canceled_task.get()
                        {
                            let mut next = prior;
                            let previous_width = next.iter().rev().find_map(|measurement| {
                                (measurement.width != width).then_some(measurement.width)
                            });
                            next.retain(|measurement| {
                                measurement.width == width
                                    || Some(measurement.width) == previous_width
                            });
                            for fresh in &measured {
                                next.retain(|measurement| {
                                    measurement.width != fresh.width || measurement.key != fresh.key
                                });
                            }
                            next.extend(measured);
                            measurements.set(next);
                        }
                    });
                }
                move || canceled.set(true)
            },
        );
    }
}

fn prepare_measurement_element(
    document: &web_sys::Document,
    track: &web_sys::Element,
    measurement_rack: &mut Option<web_sys::Element>,
    source: web_sys::Element,
) -> Option<(web_sys::HtmlElement, bool)> {
    let measurement_clone =
        source.get_attribute("data-layout-phase").as_deref() != Some("measuring");
    let element = if measurement_clone {
        source
            .clone_node_with_deep(true)
            .ok()?
            .dyn_into::<web_sys::HtmlElement>()
            .ok()?
    } else {
        source.dyn_into::<web_sys::HtmlElement>().ok()?
    };
    if measurement_clone {
        element.remove_attribute("id").ok();
        element.remove_attribute("data-mosaic-card-key").ok();
        element
            .set_attribute("data-mosaic-measurement-clone", "true")
            .expect("the fixed Mosaic measurement-clone attribute is valid");
        element
            .set_attribute("data-mosaic-measurement-active", "true")
            .expect("the fixed Mosaic measurement attribute is valid");
        element
            .set_attribute("aria-hidden", "true")
            .expect("the fixed Mosaic clone accessibility attribute is valid");
        element
            .set_attribute("inert", "")
            .expect("the fixed Mosaic clone interaction attribute is valid");
        if let Ok(ids) = element.query_selector_all("[id]") {
            for index in 0..ids.length() {
                if let Some(descendant) = ids
                    .item(index)
                    .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
                {
                    descendant.remove_attribute("id").ok();
                }
            }
        }
    }
    if !prepare_images(&element) {
        return None;
    }
    if measurement_clone {
        if measurement_rack.is_none() {
            let rack = document.create_element("div").ok()?;
            rack.set_class_name("lane-mosaic-measurements");
            track.append_child(&rack).ok()?;
            *measurement_rack = Some(rack);
        }
        measurement_rack.as_ref()?.append_child(&element).ok()?;
    }
    Some((element, measurement_clone))
}

async fn measure_unknown(
    host: NodeRef,
    width: i32,
    unknown: Vec<(usize, ActivityCard, Option<usize>)>,
    canceled: Rc<Cell<bool>>,
) -> Option<Vec<ActivityMeasurement>> {
    TimeoutFuture::new(0).await;
    if canceled.get() {
        return None;
    }
    let _trace = crate::browser::diagnostics::span("layout.measure", || {
        crate::browser::diagnostics::Metadata {
            cards: Some(unknown.len()),
            items: Some(unknown.len()),
            width_px: Some(width),
            ..crate::browser::diagnostics::Metadata::default()
        }
    });
    let host = host.cast::<web_sys::Element>()?;
    let track = host.query_selector(".lane-mosaic-track").ok().flatten()?;
    let document = host.owner_document()?;
    let mut measurement_rack = None;
    let geometry = crate::mosaic::Geometry::new(16.0, f64::from(width));
    let sources = host.query_selector_all("[data-mosaic-card-key]").ok()?;
    let mut pending = Vec::with_capacity(unknown.len());
    for (index, activity, latched_span) in unknown {
        let key = activity.key();
        let Some(source) = u32::try_from(index)
            .ok()
            .and_then(|index| sources.item(index))
            .and_then(|source| source.dyn_into::<web_sys::Element>().ok())
            .filter(|source| {
                source.get_attribute("data-mosaic-card-key").as_deref() == Some(key.as_str())
            })
        else {
            continue;
        };
        let Some((element, measurement_clone)) =
            prepare_measurement_element(&document, &track, &mut measurement_rack, source)
        else {
            continue;
        };
        pending.push(PendingActivityMeasurement {
            activity,
            element,
            measurement_clone,
            latched_span,
            spans: Vec::with_capacity(geometry.normal_spans().len() + 1),
            heights: Vec::with_capacity(geometry.normal_spans().len() + 1),
        });
    }
    measure_pending_tier(&mut pending, |item| {
        Some(item.activity.base_span(&geometry))
    });
    for span in geometry.normal_spans().skip(1) {
        measure_pending_tier(&mut pending, |item| {
            (!item.activity.is_boundary()
                && f64::from(item.heights[0]) >= crate::mosaic::TALL_THRESHOLD_PX)
                .then_some(span)
        });
    }
    measure_pending_tier(&mut pending, |item| {
        item.latched_span.filter(|span| !item.spans.contains(span))
    });
    for item in &pending {
        let base_span = item.activity.base_span(&geometry);
        if item.spans.last().copied() != Some(base_span) {
            set_measure_span(&item.element, base_span);
        }
    }
    let measured = pending
        .into_iter()
        .map(|item| item.finish(width, &geometry))
        .collect();
    if let Some(measurement_rack) = measurement_rack {
        let _ = track.remove_child(&measurement_rack);
    }
    Some(measured)
}

struct PendingActivityMeasurement {
    activity: ActivityCard,
    element: web_sys::HtmlElement,
    measurement_clone: bool,
    latched_span: Option<usize>,
    spans: Vec<usize>,
    heights: Vec<i32>,
}

impl PendingActivityMeasurement {
    fn finish(self, width: i32, geometry: &crate::mosaic::Geometry) -> ActivityMeasurement {
        let measured_height = |span| {
            self.spans
                .iter()
                .position(|measured| *measured == span)
                .map(|index| f64::from(self.heights[index]))
                .unwrap_or_else(|| panic!("Mosaic measured span {span} before admission"))
        };
        let candidates = if self.activity.base_span(geometry) == crate::mosaic::TRACK_COUNT {
            let height = measured_height(crate::mosaic::TRACK_COUNT);
            vec![crate::mosaic::Candidate {
                span: crate::mosaic::TRACK_COUNT,
                rows: if self.activity.is_boundary() {
                    crate::mosaic::boundary_rows_for(height, geometry.gap, geometry.module)
                } else {
                    crate::mosaic::rows_for(height, geometry.gap, geometry.module)
                },
                shape_cost: 0,
            }]
        } else {
            crate::mosaic::span_candidates(geometry, measured_height)
        };
        let latched = self
            .latched_span
            .filter(|span| candidates.iter().all(|candidate| candidate.span != *span))
            .map(|span| crate::mosaic::candidate_for_height(geometry, span, measured_height(span)));
        if self.measurement_clone
            && let Some(parent) = self.element.parent_node()
        {
            let _ = parent.remove_child(&self.element);
        }
        ActivityMeasurement {
            width,
            key: self.activity.key(),
            fingerprint: self.activity.measurement_fingerprint(),
            candidates,
            latched,
        }
    }
}

fn measure_pending_tier(
    pending: &mut [PendingActivityMeasurement],
    span_for: impl Fn(&PendingActivityMeasurement) -> Option<usize>,
) {
    let spans = pending.iter().map(&span_for).collect::<Vec<_>>();
    for (item, span) in pending.iter().zip(&spans) {
        if let Some(span) = span {
            set_measure_span(&item.element, *span);
        }
    }
    for (item, span) in pending.iter_mut().zip(spans) {
        let Some(span) = span else {
            continue;
        };
        item.spans.push(span);
        item.heights.push(item.element.offset_height());
    }
}

fn set_measure_span(element: &web_sys::HtmlElement, span: usize) {
    let span = span.to_string();
    element
        .set_attribute("data-measure-span", &span)
        .expect("the fixed Mosaic measurement attribute is valid");
    element
        .style()
        .set_property("--mosaic-measure-span", &span)
        .expect("Mosaic measurement writes its CSP-safe span property");
}

fn prepare_images(element: &web_sys::HtmlElement) -> bool {
    let Ok(images) = element.query_selector_all("img") else {
        return false;
    };
    let mut ready = true;
    for index in 0..images.length() {
        let Some(image) = images
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::HtmlImageElement>().ok())
        else {
            ready = false;
            continue;
        };
        if image.get_attribute("data-mosaic-image-state").is_none() {
            let _ = image.set_attribute("data-mosaic-image-state", "pending");
        }
    }
    ready
}

fn measured_activity(
    cards: &[ActivityCard],
    measurements: &[ActivityMeasurement],
    width: i32,
) -> Option<Vec<crate::mosaic_plane::MeasuredCard<String>>> {
    let measurement_index = MeasurementIndex::new(measurements);
    (width >= MIN_MEASURABLE_WIDTH_PX)
        .then(|| {
            cards
                .iter()
                .map(|activity| {
                    activity_measurement(&measurement_index, width, activity).map(|measurement| {
                        crate::mosaic_plane::MeasuredCard {
                            key: measurement.key.clone(),
                            candidates: measurement.candidates.clone(),
                            latched: measurement.latched,
                            content_revision: 0,
                            boundary: activity.is_boundary(),
                        }
                    })
                })
                .collect::<Option<Vec<_>>>()
        })
        .flatten()
}

#[hook]
fn use_persistent_plane(
    committed: UseStateHandle<Option<crate::mosaic_plane::PersistentLanePlane<String>>>,
    geometry: Option<crate::mosaic_plane::PlaneGeometry>,
    cards: Vec<ActivityCard>,
    measurements: Vec<ActivityMeasurement>,
) {
    use_effect_with(
        (geometry, cards, measurements),
        move |(geometry, cards, measurements)| {
            let Some(geometry) = geometry else {
                return;
            };
            let Some(mut measured) = measured_activity(cards, measurements, geometry.width())
            else {
                return;
            };
            measured.reverse();
            let next = committed.as_ref().map_or_else(
                || crate::mosaic_plane::PersistentLanePlane::new(*geometry, &measured),
                |plane| plane.reconcile(*geometry, &measured),
            );
            if committed.as_ref() != Some(&next) {
                committed.set(Some(next));
            }
        },
    );
}

fn render_activity_cards(
    properties: &MosaicPlaneProps,
    cards: &[ActivityCard],
    measurements: &[ActivityMeasurement],
    geometry: &crate::mosaic::Geometry,
    plane: Option<&crate::mosaic_plane::PersistentLanePlane<String>>,
) -> Html {
    let placements = plane
        .into_iter()
        .flat_map(crate::mosaic_plane::PersistentLanePlane::cards)
        .map(|entry| (entry.key.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let measurement_index = MeasurementIndex::new(measurements);
    cards
        .iter()
        .enumerate()
        .map(|(order, activity)| {
            let key = activity.key();
            let placement = plane.zip(placements.get(key.as_str()).copied());
            let measurement = placement
                .and_then(|(plane, _)| {
                    measurement_index.get(&key, plane.geometry().width(), activity)
                })
                .or_else(|| measurement_index.latest(&key));
            let layout = match (placement, measurement) {
                (Some((_plane, entry)), Some(measurement)) => {
                    CardLayout::placed(entry.card.creation_index, &entry.card, measurement)
                }
                _ => CardLayout::measuring(order, activity.base_span(geometry)),
            };
            let quote_order = if layout.committed {
                u64::try_from(layout.measurement_index)
                    .expect("Mosaic creation index fits the quote ordering field")
            } else {
                u64::MAX
                    - u64::try_from(order).expect("Mosaic card count fits the quote ordering field")
            };
            match activity {
                ActivityCard::Agent {
                    accent,
                    member,
                    quote_agents,
                    quote_sources,
                    quote_tasks,
                    quote_lane,
                    message,
                    companions,
                } => {
                    let PlaneOwner::Team { lane, .. } = &properties.owner;
                    html! {
                    <AgentMessageCard
                        key={key}
                        accent={*accent}
                        member={member.clone()}
                        quote_agents={quote_agents.clone()}
                        quote_sources={quote_sources.clone()}
                        quote_tasks={quote_tasks.clone()}
                        quote_lane={*quote_lane}
                        message={message.clone()}
                        quote_dispatcher={properties.quote_dispatcher.clone()}
                        open_task={properties.open_message_task.clone()}
                        speech_queue={properties.speak.clone()}
                        speech_owner={properties.speech_owner.as_ref()
                            .filter(|owner| owner.message_key() == key.as_str())
                            .cloned()}
                        lane={*lane}
                        order={quote_order}
                        layout={Some(layout)}
                        companions={companions.clone()}
                    />
                    }
                }
                ActivityCard::Lifecycle {
                    accent,
                    member,
                    boundary,
                } => render_lifecycle_boundary(*accent, member, boundary, &key, &layout),
            }
        })
        .collect()
}

fn render_lifecycle_boundary(
    accent: usize,
    member: &crate::runtime::LaneMember,
    boundary: &crate::runtime::LaneLifecycleBoundary,
    key: &str,
    layout: &CardLayout,
) -> Html {
    let attribution =
        watchband::agent_message_label(&member.worktree, member.name.as_deref()).to_owned();
    let label = format!("{attribution} lifecycle: {}", boundary.cause());
    let link_label = format!("Link to {label}");
    let href = crate::view::card_href(key);
    html! {
        <article
            key={key.to_owned()}
            id={key.to_owned()}
            class={classes!(
                "mosaic-message-card",
                "mosaic-compaction-card",
                "mosaic-lifecycle-card",
                layout.lifecycle.as_str(),
            )}
            aria-label={label}
            tabindex="-1"
            data-card-kind="lifecycle"
            data-message-role="system"
            data-mosaic-card-key={key.to_owned()}
            data-layout-phase={if layout.committed { "committed" } else { "measuring" }}
            data-agent-lane={member.lane.to_string()}
            data-agent-activity="lifecycle"
            data-accent={accent.to_string()}
            data-accent-slot={accent.to_string()}
            data-lifecycle-occupant={boundary.occupant().map(|occupant| occupant.to_string())}
            data-lifecycle-cause={boundary.cause().to_owned()}
            data-state={layout.lifecycle.as_str()}
            data-track={layout.track.to_string()}
            data-span={layout.span.to_string()}
            data-base={layout.base.to_string()}
            data-row={layout.row.to_string()}
            data-rows={layout.rows.to_string()}
            data-measure-span={layout.measure_span.to_string()}
        >
            <span class="mosaic-compaction-rule" aria-hidden="true"></span>
            <span class="mosaic-compaction-label">
                <Age
                    authored={boundary.observed().get().as_millisecond()}
                    class="mosaic-compaction-age"
                    {href}
                    label={link_label}
                />
                <span class="mosaic-compaction-context">
                    <span class="mosaic-compaction-agent" title={attribution.clone()}>{attribution}</span>
                    <span class="message-key">{boundary.cause().to_owned()}</span>
                </span>
            </span>
            <span class="mosaic-compaction-rule" aria-hidden="true"></span>
        </article>
    }
}
