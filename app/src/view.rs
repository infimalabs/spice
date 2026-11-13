//! Durable app locations: the one route grammar, its normalization and
//! history rule, and retained-card targets.

#[cfg(any(target_arch = "wasm32", test))]
const CREATOR_CONTEXT_TARGET_PREFIX: &str = "creator-context-";

/// A task-creation boundary encoded as one canonical Chat route target.
///
/// Source identity is part of both the route and resolution. A lane can host
/// successive transcripts whose byte loci overlap; lane and locus alone are
/// never creator authority.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CreatorContextTarget {
    pub(crate) lane: wire::CanonicalUuid,
    source: wire::CanonicalUuid,
    pub(crate) before: wire::body::TranscriptPosition,
}

#[cfg(any(target_arch = "wasm32", test))]
impl CreatorContextTarget {
    #[must_use]
    pub(crate) fn from_creation(context: &wire::body::TaskCreationContext) -> Option<Self> {
        let wire::body::TaskCreationContext::Transcript { boundary, .. } = context else {
            return None;
        };
        Some(Self {
            lane: boundary.lane,
            source: boundary.source,
            before: boundary.next,
        })
    }

    #[must_use]
    pub(crate) fn parse(target: &str) -> Option<Self> {
        let encoded = target.strip_prefix(CREATOR_CONTEXT_TARGET_PREFIX)?;
        let lane = encoded.get(..36)?;
        let encoded = encoded.get(36..)?.strip_prefix('-')?;
        let source = encoded.get(..36)?;
        let position = encoded.get(36..)?;
        let (member, offset) = position
            .strip_prefix('-')?
            .split_once('-')
            .and_then(|(member, offset)| Some((member.parse().ok()?, offset.parse().ok()?)))?;
        Some(Self {
            lane: lane.parse().ok()?,
            source: source.parse().ok()?,
            before: wire::body::TranscriptPosition { member, offset },
        })
    }

    #[must_use]
    pub(crate) fn target(&self) -> String {
        format!(
            "{CREATOR_CONTEXT_TARGET_PREFIX}{}-{}-{}-{}",
            self.lane, self.source, self.before.member, self.before.offset,
        )
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    fn owns_source(&self, message: &wire::AgentMessage) -> bool {
        self.owns_source_parts(
            message.lane,
            message
                .quote_target
                .as_ref()
                .and_then(wire::QuoteTarget::tape_source),
        )
    }

    #[must_use]
    fn owns_source_parts(
        &self,
        lane: wire::CanonicalUuid,
        source: Option<wire::CanonicalUuid>,
    ) -> bool {
        lane == self.lane && source == Some(self.source)
    }

    /// Whether one visible message from the exact source lies before this
    /// task's admission boundary.
    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) fn contains(&self, message: &wire::AgentMessage) -> bool {
        self.owns_source(message) && self.contains_locus(message.lane, message.locus)
    }

    #[must_use]
    fn contains_locus(&self, lane: wire::CanonicalUuid, locus: wire::MessageLocus) -> bool {
        lane == self.lane && (locus.member, locus.offset) < (self.before.member, self.before.offset)
    }
}

#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub(crate) fn creator_context_href<'a>(
    context: &wire::body::TaskCreationContext,
    messages: impl IntoIterator<Item = &'a wire::AgentMessage>,
) -> Option<String> {
    let target = CreatorContextTarget::from_creation(context)?;
    messages
        .into_iter()
        .any(|message| target.owns_source(message))
        .then(|| card_href(&target.target()))
}

#[cfg(any(target_arch = "wasm32", test))]
mod history;
#[cfg(any(target_arch = "wasm32", test))]
mod presentation;
#[cfg(any(target_arch = "wasm32", test))]
mod work_options;
#[cfg(any(target_arch = "wasm32", test))]
mod work_place;

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) use history::{HistoryEffect, NavigationCause, history_effect};
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) use presentation::{ChatAnchor, EntryState, GraphView, JobsView, Presentation, WorkRow};
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) use work_options::{WorkLens, WorkOptions};
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) use work_place::{TaskView, WorkLocation, WorkPlace, WorkSelection};

/// One admitted durable app URL state: the sole semantic navigation
/// authority.
///
/// Chat is always mounted. A Work route opens its overview, task, or
/// selection over it, and the Spice route opens the Spice panel over it.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum AppRoute {
    Chat { card: Option<String> },
    Work(WorkLocation),
    Spice,
}

/// One durable image locator layered over an app route.
///
/// The viewer owns the locator's grammar. Routing only bounds and
/// percent-encodes this opaque identity.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ImageTarget(String);

#[cfg(any(target_arch = "wasm32", test))]
impl ImageTarget {
    const MAX_BYTES: usize = 1024;

    #[must_use]
    pub(crate) fn new(value: String) -> Option<Self> {
        (!value.is_empty()
            && value.len() <= Self::MAX_BYTES
            && !value.chars().any(char::is_control))
        .then_some(Self(value))
    }

    #[must_use]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identity carried by one durable Work scope route.
///
/// Team membership is deliberately absent: current topology resolves it after
/// hydration.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkRoute {
    AllOpen,
    All,
    Topic(WorkRouteTopic),
    Unplaced,
    Planning { topic: Option<WorkRouteTopic> },
    Drafts,
    Team(wire::CanonicalUuid),
}

/// One exact topic label in a Work route.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkRouteTopic(String);

#[cfg(any(target_arch = "wasm32", test))]
impl WorkRouteTopic {
    #[must_use]
    pub(crate) fn new(topic: String) -> Option<Self> {
        valid_work_topic(&topic).then_some(Self(topic))
    }

    #[must_use]
    pub(crate) fn topic(&self) -> &str {
        &self.0
    }
}

/// Settled evidence for the data a durable route names.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RouteEvidence {
    pub(crate) card: Option<bool>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl AppRoute {
    #[must_use]
    pub(crate) const fn chat() -> Self {
        Self::Chat { card: None }
    }

    pub(crate) fn card(target: String) -> Option<Self> {
        valid_card_target(&target).then_some(Self::Chat { card: Some(target) })
    }

    /// One Work scope with default adjustments.
    #[must_use]
    pub(crate) fn work(route: WorkRoute) -> Self {
        Self::work_place(WorkPlace::Scope(route))
    }

    /// One Work place with default adjustments.
    #[must_use]
    pub(crate) fn work_place(place: WorkPlace) -> Self {
        Self::Work(WorkLocation {
            place,
            options: WorkOptions::default(),
        })
    }

    /// Whether this route opens a panel over Chat.
    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) const fn is_panel(&self) -> bool {
        !matches!(self, Self::Chat { .. })
    }

    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) fn card_target(&self) -> Option<&str> {
        match self {
            Self::Chat { card } => card.as_deref(),
            Self::Work(_) | Self::Spice => None,
        }
    }

    /// The task and expanded view this route opens, if it opens one task.
    #[cfg(any(target_arch = "wasm32", test, rust_analyzer))]
    #[must_use]
    pub(crate) const fn task(&self) -> Option<(&wire::TaskStamp, Option<TaskView>)> {
        match self {
            Self::Work(WorkLocation {
                place: WorkPlace::Task { task, view },
                ..
            }) => Some((task, *view)),
            Self::Work(_) | Self::Chat { .. } | Self::Spice => None,
        }
    }

    /// The overview adjustments a Work route carries.
    #[cfg(any(target_arch = "wasm32", rust_analyzer))]
    #[must_use]
    pub(crate) const fn options(&self) -> Option<&WorkOptions> {
        match self {
            Self::Work(location) => Some(&location.options),
            Self::Chat { .. } | Self::Spice => None,
        }
    }

    /// This route with `options` adjusted; routes without Work adjustments
    /// are unchanged.
    #[must_use]
    pub(crate) fn with_options(mut self, options: WorkOptions) -> Self {
        if let Self::Work(location) = &mut self {
            location.options = options;
        }
        self
    }

    /// The one canonical location spelling this route: a path, then any
    /// non-default fragment query.
    #[must_use]
    pub(crate) fn path(&self) -> String {
        match self {
            Self::Chat { card: None } => "/chat".to_owned(),
            Self::Chat { card: Some(card) } => format!("/chat/{CARD_SEGMENT}/{card}"),
            Self::Work(location) => location.path(),
            Self::Spice => SPICE_PATH.to_owned(),
        }
    }

    /// Refuse a missing Chat card back to plain Chat.
    #[must_use]
    pub(crate) fn unavailable(&self, evidence: RouteEvidence) -> Option<(Self, String)> {
        let Self::Chat { card: Some(card) } = self else {
            return None;
        };
        (evidence.card == Some(false)).then(|| {
            (
                Self::chat(),
                format!("Linked card {card} is unavailable in retained Chat history; opened Chat."),
            )
        })
    }

    /// Refuse tasks hydrated authority proves `missing` to the nearest view
    /// still containing the rest, never widening an explicit selection: a
    /// missing single task opens all open work with the same adjustments, a
    /// selection keeps its exact remaining roots, and a selection with none
    /// left opens Chat. `None` when nothing this route names is missing.
    #[must_use]
    pub(crate) fn without_missing_tasks(
        &self,
        missing: impl Fn(&wire::TaskStamp) -> bool,
    ) -> Option<(Self, String)> {
        let Self::Work(location) = self else {
            return None;
        };
        match &location.place {
            WorkPlace::Task { task, .. } if missing(task) => Some((
                Self::Work(WorkLocation {
                    place: WorkPlace::Scope(WorkRoute::AllOpen),
                    options: location.options.clone(),
                }),
                format!("Task {task} is unavailable; opened all open work."),
            )),
            WorkPlace::Selection(selection) => {
                let (gone, kept): (Vec<_>, Vec<_>) = selection
                    .tasks()
                    .iter()
                    .cloned()
                    .partition(|task| missing(task));
                if gone.is_empty() {
                    return None;
                }
                let gone = gone
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                let related = selection.related().filter(|task| kept.contains(task));
                Some(match WorkSelection::new(kept, related.cloned()) {
                    Some(remaining) => (
                        Self::Work(WorkLocation {
                            place: WorkPlace::Selection(remaining),
                            options: location.options.clone(),
                        }),
                        format!("Selected tasks {gone} are unavailable; kept the rest."),
                    ),
                    None => (
                        Self::chat(),
                        format!("Selected tasks {gone} are unavailable; opened Chat."),
                    ),
                })
            }
            WorkPlace::Task { .. } | WorkPlace::Scope(_) => None,
        }
    }
}

/// One route parsed from a location, with the parts it ignored.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug)]
struct ParsedRoute {
    route: AppRoute,
    image: Option<ImageTarget>,
    /// Segments and query pairs no route carries, as written.
    ignored: Vec<String>,
}

/// Parse one location fragment. Spelling variants a route can carry, such
/// as the room view, duplicate roots, option order, or explicit defaults,
/// parse to that route; a malformed path or empty selection is no route.
#[cfg(any(target_arch = "wasm32", test))]
fn parse_route(fragment: &str) -> Option<ParsedRoute> {
    let (path, query) = match fragment.split_once('?') {
        Some((path, query)) => (path, query),
        None => (fragment, ""),
    };
    let (query, image, mut ignored) = extract_image_query(query);
    if path == "/work" || path.starts_with("/work/") {
        let (location, route_ignored) = WorkLocation::parse(path, &query)?;
        ignored.extend(route_ignored);
        return Some(ParsedRoute {
            route: AppRoute::Work(location),
            image,
            ignored,
        });
    }
    if !query.is_empty() {
        return None;
    }
    let route = match path.strip_prefix('/')?.split('/').collect::<Vec<_>>()[..] {
        ["chat"] => AppRoute::chat(),
        ["chat", CARD_SEGMENT, target] => AppRoute::card(target.to_owned())?,
        ["spice"] => AppRoute::Spice,
        _ => return None,
    };
    Some(ParsedRoute {
        route,
        image,
        ignored,
    })
}

#[cfg(any(target_arch = "wasm32", test))]
const IMAGE_QUERY: &str = "image";

/// Remove the viewer's one orthogonal query pair before the page route reads
/// its own options. Duplicate or malformed viewer pairs are named as ignored.
#[cfg(any(target_arch = "wasm32", test))]
fn extract_image_query(query: &str) -> (String, Option<ImageTarget>, Vec<String>) {
    let mut route = Vec::new();
    let mut image = None;
    let mut ignored = Vec::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let decoded = pair.split_once('=').and_then(|(key, value)| {
            Some((
                work_options::decode_component(key)?,
                work_options::decode_component(value)?,
            ))
        });
        if let Some((key, value)) = decoded
            && key == IMAGE_QUERY
        {
            if image.is_some() {
                ignored.push(pair.to_owned());
            } else if let Some(target) = ImageTarget::new(value) {
                image = Some(target);
            } else {
                ignored.push(pair.to_owned());
            }
        } else {
            route.push(pair);
        }
    }
    (route.join("&"), image, ignored)
}

#[cfg(any(target_arch = "wasm32", test))]
impl WorkRoute {
    #[must_use]
    pub(crate) fn path(&self) -> String {
        match self {
            Self::AllOpen => "/work".to_owned(),
            Self::All => "/work/all".to_owned(),
            Self::Topic(topic) => format!("/work/topic{}", topic.topic()),
            Self::Unplaced => "/work/unplaced".to_owned(),
            Self::Planning { topic: None } => "/work/planning".to_owned(),
            Self::Planning { topic: Some(topic) } => format!("/work/planning{}", topic.topic()),
            Self::Drafts => "/work/drafts".to_owned(),
            Self::Team(team) => format!("/work/team/{team}"),
        }
    }

    /// Parse one exact scope path.
    fn parse(path: &str) -> Option<Self> {
        if let Some(topic) = path.strip_prefix("/work/topic") {
            return WorkRouteTopic::new(topic.to_owned()).map(Self::Topic);
        }
        if let Some(topic) = path
            .strip_prefix("/work/planning")
            .filter(|topic| !topic.is_empty())
        {
            return Some(Self::Planning {
                topic: Some(WorkRouteTopic::new(topic.to_owned())?),
            });
        }
        match path.strip_prefix("/work")? {
            "" => Some(Self::AllOpen),
            "/all" => Some(Self::All),
            "/unplaced" => Some(Self::Unplaced),
            "/planning" => Some(Self::Planning { topic: None }),
            "/drafts" => Some(Self::Drafts),
            scope => Some(Self::Team(scope.strip_prefix("/team/")?.parse().ok()?)),
        }
    }
}

/// One exact absolute public topic path.
#[cfg(any(target_arch = "wasm32", test))]
fn valid_work_topic(topic: &str) -> bool {
    topic.parse::<wire::Topic>().is_ok()
}

#[cfg(any(target_arch = "wasm32", test))]
const CARD_SEGMENT: &str = "card";
#[cfg(any(target_arch = "wasm32", test))]
const SPICE_PATH: &str = "/spice";
#[cfg(any(target_arch = "wasm32", rust_analyzer, test))]
const TEAM_STREAM_CARD_PREFIX: &str = "team-stream-";

#[cfg(any(target_arch = "wasm32", rust_analyzer, test))]
pub(crate) fn team_stream_card_target(team: wire::CanonicalUuid) -> String {
    format!("{TEAM_STREAM_CARD_PREFIX}{team}")
}

#[cfg(any(target_arch = "wasm32", rust_analyzer, test))]
pub(crate) fn team_stream_card_team(target: &str) -> Option<wire::CanonicalUuid> {
    target.strip_prefix(TEAM_STREAM_CARD_PREFIX)?.parse().ok()
}

/// One browser location resolved to a usable app route.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NormalizedAppLocation {
    route: AppRoute,
    image: Option<ImageTarget>,
    replace: bool,
    notice: Option<String>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl NormalizedAppLocation {
    #[must_use]
    pub(crate) const fn route(&self) -> &AppRoute {
        &self.route
    }

    #[must_use]
    pub(crate) const fn image(&self) -> Option<&ImageTarget> {
        self.image.as_ref()
    }

    /// Whether the location must be rewritten in place to the route's
    /// canonical spelling before it is read.
    #[must_use]
    pub(crate) const fn needs_replace(&self) -> bool {
        self.replace
    }

    #[must_use]
    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }
}

#[cfg(any(target_arch = "wasm32", test))]
#[must_use]
pub(crate) fn image_path(route: &AppRoute, image: Option<&ImageTarget>) -> String {
    let mut path = route.path();
    if let Some(image) = image {
        path.push(if path.contains('?') { '&' } else { '?' });
        path.push_str(IMAGE_QUERY);
        path.push('=');
        path.push_str(&work_options::encode_component(image.as_str()));
    }
    path
}

#[cfg(any(target_arch = "wasm32", test))]
const OPENING_AUTH_TOKEN_MIN_BYTES: usize = 16;
#[cfg(any(target_arch = "wasm32", test))]
const OPENING_AUTH_TOKEN_MAX_BYTES: usize = 256;

/// The one-time bearer token carried by an authenticated app opening.
///
/// This mirrors the resident's published URL-safe token contract. Canonical
/// routes contain a slash and shorthand route spellings are below its minimum.
#[cfg(any(target_arch = "wasm32", test))]
#[must_use]
pub(crate) fn opening_auth_token(hash: &str) -> Option<&str> {
    let token = hash.strip_prefix('#')?;
    app_auth_token_is_valid(token).then_some(token)
}

#[cfg(any(target_arch = "wasm32", test))]
#[must_use]
pub(crate) fn app_auth_token_is_valid(token: &str) -> bool {
    (OPENING_AUTH_TOKEN_MIN_BYTES..=OPENING_AUTH_TOKEN_MAX_BYTES).contains(&token.len())
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~'))
}

/// Resolve a browser query and hash through the one route grammar. Any
/// spelling other than the route's canonical one is replaced in place; a
/// recorded Design permalink becomes its task's address; ignored parts and
/// unrecognized locations are named in a notice.
#[cfg(any(target_arch = "wasm32", test))]
#[must_use]
pub(crate) fn normalize_app_location(search: &str, hash: &str) -> NormalizedAppLocation {
    let fragment = hash.strip_prefix('#').unwrap_or(hash);
    if search.is_empty() {
        if let Some(task) = fragment
            .strip_prefix(DESIGN_PREFIX)
            .and_then(|stamp| stamp.parse::<wire::TaskStamp>().ok())
        {
            return NormalizedAppLocation {
                route: AppRoute::work_place(WorkPlace::Task { task, view: None }),
                image: None,
                replace: true,
                notice: None,
            };
        }
        if let Some(ParsedRoute {
            route,
            image,
            ignored,
        }) = parse_route(fragment)
        {
            let canonical = image_path(&route, image.as_ref());
            let replace = hash.strip_prefix('#') != Some(canonical.as_str());
            let notice = (!ignored.is_empty()).then(|| {
                format!(
                    "Ignored `{}` in route `{hash}`; opened {canonical}.",
                    ignored.join("`, `"),
                )
            });
            return NormalizedAppLocation {
                route,
                image,
                replace,
                notice,
            };
        }
        let name = fragment.trim_matches('/');
        if !name.contains('/') && matches!(name, "" | "chat") {
            return NormalizedAppLocation {
                route: AppRoute::chat(),
                image: None,
                replace: true,
                notice: None,
            };
        }
    }

    NormalizedAppLocation {
        route: AppRoute::chat(),
        image: None,
        replace: true,
        notice: Some(format!("Unrecognized route `{search}{hash}`; opened Chat.")),
    }
}

/// The recorded location prefix of a Design permalink, followed by its birth
/// stamp. Such a link canonicalizes to its task address.
#[cfg(any(target_arch = "wasm32", test))]
const DESIGN_PREFIX: &str = "/chat/design/";

/// Link one Design's read view, its task address, by its immutable birth
/// stamp.
#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub(crate) fn design_href(design: &wire::TaskStamp) -> String {
    let route = AppRoute::work_place(WorkPlace::Task {
        task: design.clone(),
        view: None,
    });
    format!("#{}", route.path())
}

#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub(crate) fn card_href(target: &str) -> String {
    let route = AppRoute::card(target.to_owned())
        .expect("a rendered app card has a canonical route target");
    format!("#{}", route.path())
}

/// Link one retained quote source to its durable Chat card.
#[cfg(any(target_arch = "wasm32", rust_analyzer))]
pub(crate) fn quote_source_href(source: &str) -> String {
    card_href(source)
}

#[cfg(any(target_arch = "wasm32", test))]
fn valid_card_target(target: &str) -> bool {
    !target.is_empty()
        && target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[cfg(test)]
mod tests;
