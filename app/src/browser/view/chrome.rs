//! The chrome standing above the feed.
//!
//! The main composer, Help, work totals, and Spice launcher stand across the
//! top.
//! Lane standings, the newest command outcome, and feed cards reuse the
//! derived watchband projections below them.
//!
//! The connection state lives in this bar because this bar is the one piece of
//! chrome every layout renders. The surface keeps the last admitted runtime on
//! screen through an outage and through a restarted resident's hydration, so a
//! layout that hid the state would show a live-looking surface whose composers
//! accept nothing.

use super::*;
use crate::browser::clock::Age;
use crate::watchband;
use crate::work::{WorkCounts, WorkMark, WorkProjection, WorkScope};
/// Characters the ticker's `aria-live` label announces.
pub(in crate::browser) const TICKER_SPOKEN_BUDGET: usize = 160;

/// The fixed command bar and its browser-local sidebar launcher.
pub(super) struct CommandBarRender<'a> {
    pub(super) connection: &'a UseStateHandle<Connection>,
    pub(super) navigate: &'a Callback<navigation::Intent>,
    pub(super) spice_sidebar_open: bool,
    pub(super) open_spice_sidebar: &'a Callback<web_sys::Element>,
    pub(super) work: Option<&'a WorkProjection<'a>>,
    pub(super) app_update: &'a UseStateHandle<Option<AppUpdate>>,
    pub(super) surface: &'a SurfaceData,
    pub(super) composer: Html,
    pub(super) sender: &'a SenderHandle,
}

pub(super) fn render_command_bar(render: CommandBarRender<'_>) -> Html {
    let surface = render.surface.read();
    let runtime = surface.as_ref();
    let connection_label: AttrValue = render.connection.label().into();
    html! {
        <header class="commandbar commandbar--mosaic">
            <div class="app-commandbar-content">
                // Connection is routine state: the Spice button, tab title, and
                // icon carry it visibly; only assistive technology hears it.
                <span class="visually-hidden" role="status">{connection_label.clone()}</span>
                <div class="app-compose-lane">
                    <crate::browser::help::Help connection={(**render.connection).clone()}
                        on_action={{
                            let sender = render.sender.clone();
                            Callback::from(move |action| admit_action(&sender, action))
                        }}>
                        {render.composer}
                    </crate::browser::help::Help>
                </div>
                <nav class="app-work-totals" aria-label="Open work">
                    {render_drafts_total(render.work, render.navigate)}
                    {render_work_total(render.work, render.navigate, GlobalWorkRail::Execution)}
                    {render_work_total(render.work, render.navigate, GlobalWorkRail::Design)}
                </nav>
                {render_spice_button(SpiceButton {
                    connection: render.connection,
                    app_update: (**render.app_update).as_ref(),
                    runtime,
                    place: SpicePlace::Launcher {
                        expanded: render.spice_sidebar_open,
                    },
                    node_ref: NodeRef::default(),
                    onclick: {
                        let open = render.open_spice_sidebar.clone();
                        Callback::from(move |event: MouseEvent| {
                            if let Some(target) = event
                                .current_target()
                                .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
                            {
                                open.emit(target);
                            }
                        })
                    },
                })}
            </div>
        </header>
    }
}

/// One Spice button: the command bar's launcher, or the open Spice sidebar's
/// close control standing at that same place.
pub(super) struct SpiceButton<'a> {
    pub(super) connection: &'a Connection,
    pub(super) app_update: Option<&'a AppUpdate>,
    pub(super) runtime: Option<&'a AppRuntime>,
    pub(super) place: SpicePlace,
    pub(super) node_ref: NodeRef,
    pub(super) onclick: Callback<MouseEvent>,
}

#[derive(Clone, Copy)]
pub(super) enum SpicePlace {
    /// In the command bar, opening the sidebar and receiving focus back.
    Launcher { expanded: bool },
    /// In the open sidebar over the launcher's place, closing it.
    Anchor,
}

pub(super) fn render_spice_button(button: SpiceButton<'_>) -> Html {
    let fast_mode = button
        .runtime
        .and_then(AppRuntime::settings)
        .is_some_and(|settings| settings.fast_mode);
    let (verb, expanded, focus_key) = match button.place {
        SpicePlace::Launcher { expanded } => ("Open", expanded, Some("spice")),
        SpicePlace::Anchor => ("Close", true, None),
    };
    let trigger_label = match (button.app_update, fast_mode) {
        (Some(_), true) => format!("{verb} Spice - update available; fast mode on"),
        (Some(_), false) => format!("{verb} Spice - update available"),
        (None, true) => format!("{verb} Spice - fast mode on"),
        (None, false) => format!("{verb} Spice"),
    };
    let quota = quota_level(button.runtime);
    let edge_accent = match (fast_mode, button.app_update.is_some()) {
        (false, false) => None,
        (true, false) => Some(3),
        (false, true) => Some(1),
        (true, true) => Some(3),
    };
    let connection_label = button.connection.label();
    let trigger_label: AttrValue = match &quota {
        Some(quota) => format!("{trigger_label} - {connection_label} - {}", quota.label),
        None => format!("{trigger_label} - {connection_label}"),
    }
    .into();
    html! {
        <button
            ref={button.node_ref}
            class="app-spice-button"
            type="button"
            data-accent={edge_accent.map(|accent| accent.to_string())}
            data-connection={button.connection.indicator_state()}
            data-quota-stale={quota.as_ref().is_some_and(|quota| quota.stale).then_some("true")}
            data-sidebar-focus-key={focus_key}
            aria-label={trigger_label.clone()}
            aria-haspopup="dialog"
            aria-expanded={expanded.to_string()}
            aria-controls="app-sidebar-title"
            title={trigger_label}
            onclick={button.onclick}
        >
            <span class="app-spice-well" aria-hidden="true">
                {quota.as_ref().map(QuotaLevel::render)}
            </span>
            <span class="app-spice-pepper" aria-hidden="true">{"🌶️"}</span>
        </button>
    }
}

/// Open inputs in the connected principal's inbox, counted only while the
/// room projection that places them is available, as the inbox itself counts
/// them.
pub(in crate::browser) fn waiting_messages(runtime: &AppRuntime) -> usize {
    if runtime.room_projection().status() == crate::runtime::RoomProjectionStatus::Available {
        runtime.principal_inbox().len()
    } else {
        0
    }
}

/// Plan-weighted weekly quota drawn as account liquid layers, ordered by
/// remaining capacity, each in the accent its capacity order assigns.
struct QuotaLevel<'a> {
    stale: bool,
    stack: crate::quota::WeeklyQuotaStack<'a>,
    label: String,
}

impl QuotaLevel<'_> {
    /// SVG geometry attributes, unlike inline style, survive the app's
    /// `style-src 'self'` policy.
    fn render(&self) -> Html {
        let mut surface = 100.0;
        let layers = self
            .stack
            .layers()
            .into_iter()
            .map(|layer| {
                let depth = layer.available / self.stack.capacity * 100.0;
                surface = (surface - depth).max(0.0);
                html! {
                    <rect
                        class="app-spice-quota-account"
                        data-accent={(layer.accent % watchband::ACCENTS).to_string()}
                        x="0"
                        y={format!("{surface:.3}")}
                        width="100"
                        height={format!("{depth:.3}")}
                    />
                }
            })
            .collect::<Html>();
        html! {
            <svg class="app-spice-quota-well" viewBox="0 0 100 100" preserveAspectRatio="none" focusable="false">
                {layers}
            </svg>
        }
    }
}

/// Only a failed refresh marks the level stale.
fn quota_level(runtime: Option<&AppRuntime>) -> Option<QuotaLevel<'_>> {
    let generation = runtime.and_then(AppRuntime::quota)?;
    let stack = crate::quota::weekly_quota_stack(generation.last_good.as_deref()?)?;
    let stale = crate::quota::last_good_stale(generation);
    let percentage = stack.remaining * 100.0;
    let label = if stale {
        format!(
            "weekly quota {percentage:.0}% remaining ({})",
            crate::quota::Condition::StaleLastGood.label()
        )
    } else {
        format!("weekly quota {percentage:.0}% remaining")
    };
    Some(QuotaLevel {
        stale,
        stack,
        label,
    })
}

/// One neutral task count badge shared by global and local roll-ups.
pub(in crate::browser) fn render_task_count(count: usize) -> Html {
    html! {
        <span class="app-count-badge">{count.to_string()}</span>
    }
}

/// Every open category a Work count title names, in task status's terms.
fn count_breakdown(counts: WorkCounts) -> String {
    format!(
        "{} ready, {} review, {} active or in flight, {} stale, {} blocked, {} waiting or set aside",
        counts.ready(),
        counts.review(),
        counts.in_flight(),
        counts.stale(),
        counts.blocked(),
        counts.waiting(),
    )
}

/// A Work tab's mark: the most actionable state the partition it opens holds,
/// or none.
pub(in crate::browser) fn work_mark(counts: WorkCounts) -> Option<&'static str> {
    counts.mark().map(|(mark, _)| mark.key())
}

fn task_count_title_with_extra(
    label: &str,
    counts: WorkCounts,
    extra: usize,
    extra_label: &str,
) -> String {
    let total = counts.open() + extra;
    let breakdown = if extra == 0 {
        count_breakdown(counts)
    } else {
        format!(
            "{}, {extra} {extra_label}{}",
            count_breakdown(counts),
            if extra == 1 { "" } else { "s" },
        )
    };
    let title = format!(
        "{label}: {total} {}; {breakdown}",
        if total == 1 { "task" } else { "tasks" },
    );
    match counts.mark() {
        None => title,
        Some((WorkMark::Blocked, count)) => format!("{count} need you; {title}"),
        Some((WorkMark::Decision, count)) => {
            format!("{count} awaiting your decision; {title}")
        }
        Some((WorkMark::Pass, count)) => format!("{count} ready for a research pass; {title}"),
        Some((WorkMark::Deferred, count)) => format!("{count} deferred; {title}"),
    }
}

/// A global Work tab's title and accessible name, led by the count of tasks in
/// the state its mark shows, so that state never rests on colour alone.
pub(in crate::browser) fn task_count_title(label: &str, counts: WorkCounts) -> String {
    task_count_title_with_extra(label, counts, 0, "")
}

/// A team total's title counts the claimed Drafts its local rail also renders.
pub(in crate::browser) fn team_task_count_title(
    label: &str,
    counts: WorkCounts,
    claimed_drafts: usize,
) -> String {
    task_count_title_with_extra(label, counts, claimed_drafts, "claimed Draft")
}

/// Which global total a button counts: all open execution work, or all open
/// Design work.
#[derive(Clone, Copy)]
enum GlobalWorkRail {
    Execution,
    Design,
}

impl GlobalWorkRail {
    fn scope(self) -> WorkScope {
        match self {
            Self::Execution => WorkScope::AllOpen,
            Self::Design => WorkScope::planning(),
        }
    }

    const fn focus_key(self) -> &'static str {
        match self {
            Self::Execution => "global-work",
            Self::Design => "design-work",
        }
    }

    const fn aggregate_title(self) -> &'static str {
        match self {
            Self::Execution => "All open execution work",
            Self::Design => "All open Design work",
        }
    }

    const fn data_scope(self) -> &'static str {
        match self {
            Self::Execution => "all",
            Self::Design => "design",
        }
    }
}

/// The label-less total for one global rail, the same hanging tab as before
/// seated beside the Spice launcher; nothing while that rail holds no open
/// work. Its badge is one open-task count, matching every team total, and it
/// opens the rail's whole scope.
fn render_work_total(
    work: Option<&WorkProjection<'_>>,
    navigate: &Callback<navigation::Intent>,
    rail: GlobalWorkRail,
) -> Html {
    let scope = rail.scope();
    let Some(counts) = work
        .map(|work| work.scope_counts(&scope))
        .filter(|counts| counts.has_open())
    else {
        return Html::default();
    };
    let base_accent =
        matches!(rail, GlobalWorkRail::Design).then(|| watchband::DESIGN_BASE.to_string());
    let title = task_count_title(rail.aggregate_title(), counts);
    let focus_key = rail.focus_key();
    let navigate = navigate.clone();
    html! {
        <button
            key={focus_key}
            class="app-horn-tab app-horn-tab--down"
            type="button"
            title={title.clone()}
            aria-label={title}
            data-work-scope={rail.data_scope()}
            data-work-mark={work_mark(counts)}
            data-base-flavor={base_accent.as_ref().map(|_| "accent")}
            data-accent={base_accent}
            data-sidebar-focus-key={focus_key}
            onclick={Callback::from(move |_| navigate.emit(navigation::Intent::Work(scope.clone())))}
        >
            {render_task_count(counts.open())}
        </button>
    }
}

/// The operator's open Drafts, a hanging total in the operator's neutral base
/// beside the composer it feeds; nothing while no Draft is open. It opens the
/// Drafts Work scope, which wears the same base.
fn render_drafts_total(
    work: Option<&WorkProjection<'_>>,
    navigate: &Callback<navigation::Intent>,
) -> Html {
    let Some(work) = work else {
        return Html::default();
    };
    let count = work
        .tasks()
        .iter()
        .filter(|task| work.in_scope(task, &WorkScope::Drafts))
        .count();
    if count == 0 {
        return Html::default();
    }
    let title = format!("Drafts: {count} open");
    let navigate = navigate.clone();
    html! {
        <button
            key="drafts-work"
            class="app-horn-tab app-horn-tab--down app-drafts-total"
            type="button"
            title={title.clone()}
            aria-label={title}
            data-work-scope="drafts"
            data-base-flavor="neutral"
            data-sidebar-focus-key="drafts-work"
            onclick={Callback::from(move |_| navigate.emit(navigation::Intent::Work(WorkScope::Drafts)))}
        >
            <span class="app-count-badge">{count.to_string()}</span>
        </button>
    }
}

/// Newest activity from any member of one team.
pub(in crate::browser) fn render_team_ticker(
    runtime: &AppRuntime,
    team: &crate::runtime::LaneRow,
    standings: &[watchband::Standing],
) -> Html {
    let Some(activity) = runtime.newest_team_activity(team) else {
        return render_empty_team_ticker();
    };
    let standing = standings
        .iter()
        .find(|standing| standing.lane == activity.lane())
        .expect("a team activity belongs to one rendered standing");
    let member = team
        .member(activity.lane())
        .expect("a team activity belongs to one rendered member");
    ticker(activity, standing, member.status(standing.waking))
}

/// The ticker row before any activity, or with no team at all, keeps the
/// team surface's pane where the first activity or team leaves it.
pub(in crate::browser) fn render_empty_team_ticker() -> Html {
    html! {
        <div class="app-status-band app-status-text ticker" aria-live="polite" data-empty="true"></div>
    }
}

fn ticker(
    activity: &crate::runtime::LaneActivity,
    standing: &watchband::Standing,
    status: crate::runtime::LaneStatus,
) -> Html {
    let class = activity.title();
    let visible_prefix = if activity.class() == "prose" {
        standing.label.as_str()
    } else {
        class
    };
    let source = if activity.detail().is_empty() && class == "tool" {
        visible_prefix
    } else {
        activity.detail()
    };
    let (prefix, detail) =
        watchband::ticker_chunks(visible_prefix, source, watchband::STATUS_LINE_DOM_LIMIT);
    let separator = if activity.class() == "prose" || class == "tool" || detail.is_empty() {
        ""
    } else {
        ":"
    };
    let line = if detail.is_empty() {
        format!("{prefix}{separator}")
    } else {
        format!("{prefix}{separator} {detail}")
    };
    let accessible_label =
        watchband::ticker_accessible_label(&standing.label, class, &line, TICKER_SPOKEN_BUDGET);
    html! {
        <div
            class={classes!("app-status-band", "app-status-text", "ticker", activity.class())}
            aria-live="polite"
            aria-label={accessible_label}
            title={line}
            data-agent-lane={activity.lane().to_string()}
            data-accent={standing.accent.to_string()}
            data-lane-status={status.key()}
        >
            <Age
                authored={activity.display_millis()}
                class={classes!("ticker-age", status.key())}
                title={format!("{} {class}", standing.label)}
            />
            {" "}
            <strong class="ticker-prefix">{format!("{prefix}{separator}")}</strong>
            if !detail.is_empty() {
                {" "}
                <span class="ticker-detail">{detail}</span>
            }
        </div>
    }
}

/// Agent-card actions and the colour of its producing worktree.
#[derive(Clone, PartialEq)]
pub(in crate::browser) struct Correspondent {
    pub(in crate::browser) accent: usize,
    pub(in crate::browser) speak: Callback<(SpeechControl, String)>,
    pub(in crate::browser) speech_owner: Option<SpeechOwner>,
    pub(in crate::browser) message_key: String,
    /// Quote this card's source into its own composer or the main composer.
    pub(in crate::browser) quote: Option<Callback<crate::composer_drafts::QuoteDestination>>,
}

fn action_hunk(event: &MouseEvent) -> Option<web_sys::Element> {
    event
        .current_target()
        .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
        .and_then(|target| target.closest(".message-hunk").ok().flatten())
}

/// Rendered chrome around authored words: controls, attribution, image
/// status, board provenance, and portal staging.
const NOT_AUTHORED: &str = ".message-controls, .message-attribution, .message-image-status, \
     .message-quote-task-tail, .message-quote-task-line, .markdown-body-portals";

fn remove_all(nodes: Result<web_sys::NodeList, JsValue>) {
    let Ok(nodes) = nodes else {
        return;
    };
    for index in 0..nodes.length() {
        if let Some(element) = nodes
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        {
            element.remove();
        }
    }
}

/// A hunk clone containing only the authored words an action consumes.
fn authored_hunk(hunk: &web_sys::Element) -> Option<web_sys::Element> {
    let clone = hunk
        .clone_node_with_deep(true)
        .ok()?
        .dyn_into::<web_sys::Element>()
        .ok()?;
    remove_all(clone.query_selector_all(NOT_AUTHORED));
    Some(clone)
}

fn action_speech_text(event: &MouseEvent, fallback: &str) -> String {
    action_hunk(event)
        .and_then(|hunk| authored_hunk(&hunk))
        .and_then(|hunk| hunk.text_content())
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// The card's outermost rendered Markdown body holding `hunk`.
fn card_body(hunk: &web_sys::Element) -> Option<web_sys::Element> {
    let card = hunk.closest(".mosaic-message-card").ok().flatten()?;
    let mut body = None;
    let mut node = hunk.parent_element();
    while let Some(element) = node.filter(|element| *element != card) {
        if element.matches(".message-body").unwrap_or(false) {
            body = Some(element.clone());
        }
        node = element.parent_element();
    }
    body
}

/// Rendered words from this hunk through the end of its card, quotes outside
/// any hunk included. The temporary off-screen host lets `innerText` preserve
/// block and list line breaks; chrome and nested cards stay out. Each rendered
/// blockquote line regains one `>` per nesting depth. The message's own text
/// stands in when no words rendered.
fn action_copy_text(event: &MouseEvent, fallback: &str) -> String {
    let text = (|| {
        let hunk = action_hunk(event)?;
        let card_body = card_body(&hunk)?;
        let document = hunk.owner_document()?;
        let range = document.create_range().ok()?;
        range.set_start_before(&hunk).ok()?;
        range
            .set_end(&card_body, card_body.child_nodes().length())
            .ok()?;
        let authored = range.clone_contents().ok()?;
        remove_all(authored.query_selector_all(NOT_AUTHORED));
        remove_all(authored.query_selector_all(".mosaic-message-card"));
        let body = document.body()?;
        let host = document
            .create_element("div")
            .ok()?
            .dyn_into::<web_sys::HtmlElement>()
            .ok()?;
        host.set_attribute("aria-hidden", "true").ok()?;
        let style = host.style();
        style.set_property("position", "fixed").ok()?;
        style.set_property("inset-inline-start", "-100000px").ok()?;
        style.set_property("inset-block-start", "0").ok()?;
        style.set_property("width", "40rem").ok()?;
        style.set_property("opacity", "0").ok()?;
        style.set_property("pointer-events", "none").ok()?;
        host.append_child(&authored).ok()?;
        body.append_child(&host).ok()?;
        mark_quotes(&host);
        let text = host.inner_text();
        host.remove();
        Some(text)
    })()
    .unwrap_or_default();
    copied_text(&text, fallback)
}

/// What one Copy puts on the clipboard: the rendered words, or `fallback` when
/// none rendered, ending in exactly one newline so a paste completes its line.
/// Nothing to copy stays empty.
fn copied_text(rendered: &str, fallback: &str) -> String {
    let words = match rendered.trim() {
        "" => fallback.trim(),
        words => words,
    };
    if words.is_empty() {
        String::new()
    } else {
        format!("{words}\n")
    }
}

/// Replace every blockquote under `host`, innermost first, with a paragraph of
/// its rendered lines prefixed by `> `, or a bare `>` for a blank line, so
/// `innerText` keeps Markdown quote depth. A paragraph's blank-line boundary
/// keeps the next line or quote from continuing it.
fn mark_quotes(host: &web_sys::HtmlElement) {
    let Some(document) = host.owner_document() else {
        return;
    };
    let Ok(quotes) = host.query_selector_all("blockquote") else {
        return;
    };
    for index in (0..quotes.length()).rev() {
        let Some(quote) = quotes
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            continue;
        };
        let marked = quote
            .inner_text()
            .trim_matches('\n')
            .lines()
            .map(|line| {
                if line.trim().is_empty() {
                    ">".to_owned()
                } else {
                    format!("> {line}")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let Some(lines) = document
            .create_element("p")
            .ok()
            .and_then(|node| node.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            return;
        };
        let _ = lines.style().set_property("white-space", "pre-wrap");
        lines.set_text_content(Some(&marked));
        let _ = quote.replace_with_with_node_1(&lines);
    }
}

#[derive(Properties, PartialEq)]
struct CardActionsProps {
    text: AttrValue,
    correspondent: Correspondent,
}

/// A rail's virtual drawer: one full-size rounded opener occupies flow and
/// slides toward the inline start to reveal every square action while the card
/// stays fixed. Every drawer shares one `name`, so opening one closes any
/// other. Choosing an action or leaving the drawer closes it; Escape closes it
/// and restores the opener's focus.
pub(in crate::browser) fn render_action_drawer(panel: Html) -> Html {
    html! {
        <details
            class="message-actions-drawer"
            name="message-actions-drawer"
            data-app-overlay-disclosure=""
            onfocusout={Callback::from(crate::browser::overlay::dismiss_disclosure_on_focusout)}
            onkeydown={Callback::from(crate::browser::overlay::dismiss_disclosure_on_escape)}
        >
            <summary class="message-actions-opener" title="More actions" aria-label="More actions">
                {Html::from_html_unchecked(AttrValue::Static(crate::composer_markdown::DRAWER_OPENER_ICON))}
            </summary>
            <span
                class="message-actions-drawer__panel"
                onclick={Callback::from(|event: MouseEvent| {
                    crate::browser::overlay::dismiss_disclosure_from_click(&event);
                })}
            >{panel}</span>
        </details>
    }
}

/// Quote into the main composer from a drawer.
pub(in crate::browser) fn render_draft_quote_action(
    quote: &Callback<crate::composer_drafts::QuoteDestination>,
    label: AttrValue,
) -> Html {
    let quote = quote.clone();
    html! {
        <button
            class="app-card-action app-card-action--draft-quote" type="button"
            title={label.clone()} aria-label={label}
            onclick={Callback::from(move |_| {
                quote.emit(crate::composer_drafts::QuoteDestination::NewDraft);
            })}
        >{Html::from_html_unchecked(AttrValue::Static(crate::composer_markdown::DRAFT_QUOTE_ICON))}</button>
    }
}

#[function_component(CardActions)]
fn card_actions(props: &CardActionsProps) -> Html {
    let control = use_context::<SpeechControl>().unwrap_or_default();
    let speak = props.correspondent.speak.clone();
    let quote = props.correspondent.quote.clone();
    let speech_fallback = props.text.clone();
    let copy_fallback = props.text.clone();
    let toaster = crate::browser::toast::use_toaster();
    let speaking = props
        .correspondent
        .speech_owner
        .as_ref()
        .is_some_and(|owner| owner.is_control(&props.correspondent.message_key, control));
    let speech_label = if speaking {
        "Stop playback"
    } else {
        "Speak this message"
    };
    let play = html! {
        <button
            class={classes!(
                "app-card-action",
                "app-card-action--speak",
                speaking.then_some("active"),
            )}
            type="button" title={speech_label}
            aria-label={speech_label}
            onclick={Callback::from(move |event: MouseEvent| {
                speak.emit((control, action_speech_text(&event, speech_fallback.as_str())));
            })}
        >{
            if speaking {
                html! {
                    <svg
                        class="app-card-action__icon"
                        data-card-action-icon="stop"
                        width="15" height="15" viewBox="0 0 24 24"
                        aria-hidden="true" focusable="false"
                    >
                        <rect x="7" y="7" width="10" height="10" rx="1.5" fill="currentColor" />
                    </svg>
                }
            } else {
                html! {
                    <svg
                        class="app-card-action__icon"
                        data-card-action-icon="play"
                        width="15" height="15" viewBox="0 0 24 24"
                        aria-hidden="true" focusable="false"
                    >
                        <path d="M8 5v14l11-7Z" fill="currentColor" />
                    </svg>
                }
            }
        }</button>
    };
    let copy = {
        Callback::from(move |event: MouseEvent| {
            let text = action_copy_text(&event, copy_fallback.as_str());
            let toaster = toaster.clone();
            crate::browser::clipboard::copy_text(&text, move |result| {
                toaster.settled(match result {
                    Ok(()) => "Copied message text".to_owned(),
                    Err(error) => format!("Copy failed: {error}"),
                });
            });
        })
    };
    html! {
        <span class="message-actions app-control-rail">
            {render_action_drawer(html! {
                <>
                    {play}
                    <button
                        class="app-card-action app-card-action--copy" type="button"
                        title="Copy from here" aria-label="Copy this message from here"
                        onclick={copy}
                    >
                        <svg
                            class="app-card-action__icon"
                            data-card-action-icon="copy"
                            width="15" height="15" viewBox="0 0 24 24"
                            aria-hidden="true" focusable="false"
                        >
                            <path
                                d="M9 9h10v10H9V9Zm1.75 1.75v6.5h6.5v-6.5h-6.5ZM5 5h10.5v1.75H6.75v8.75H5V5Z"
                                fill="currentColor" fill-rule="evenodd"
                            />
                        </svg>
                    </button>
                    if let Some(quote) = &quote {
                        {render_draft_quote_action(quote, AttrValue::Static("Quote into the main composer"))}
                        <button
                            class="app-card-action app-card-action--quote" type="button" title="Quote this message"
                            aria-label="Quote this message"
                            onclick={quote.reform(|_| crate::composer_drafts::QuoteDestination::Composer)}
                        >{Html::from_html_unchecked(AttrValue::Static(crate::composer_markdown::QUOTE_ICON))}</button>
                    }
                </>
            })}
        </span>
    }
}

pub(in crate::browser) fn render_card_actions(
    text: AttrValue,
    correspondent: &Correspondent,
) -> Html {
    html! {
        <CardActions text={text} correspondent={correspondent.clone()} />
    }
}

#[derive(Properties, PartialEq)]
pub(in crate::browser) struct TurboButtonProps {
    #[prop_or_default]
    pub(in crate::browser) class: Classes,
    #[prop_or_default]
    pub(in crate::browser) label: Option<AttrValue>,
    pub(in crate::browser) engaged: bool,
    pub(in crate::browser) disabled: bool,
    pub(in crate::browser) title: AttrValue,
    pub(in crate::browser) onclick: Callback<MouseEvent>,
}

/// Shared stable glass switch lettered TURBO; pressed state reflects team or
/// global fast mode. The word never changes: engaged, the button's glass
/// backlights in amber the way a lit slider does, and the pressed state and
/// label stay the accessible truth.
#[function_component(TurboButton)]
pub(in crate::browser) fn turbo_button(props: &TurboButtonProps) -> Html {
    let label = props
        .label
        .clone()
        .unwrap_or_else(|| AttrValue::from("Turbo"));
    html! {
        <button
            type="button"
            class={classes!("app-turbo-button", props.class.clone())}
            data-lit={props.engaged.to_string()}
            aria-label={label}
            aria-pressed={props.engaged.to_string()}
            title={props.title.clone()}
            disabled={props.disabled}
            onclick={props.onclick.clone()}
        >
            {fit_word("Turbo")}
        </button>
    }
}

/// One control word drawn to fill its box exactly: every word occupies the
/// same physical width and height, squeezed or stretched to fit.
pub(in crate::browser) fn fit_word(word: &'static str) -> Html {
    html! {
        <svg class="fit-word" viewBox="0 0 100 16" preserveAspectRatio="none" aria-hidden="true">
            <text x="0" y="13" font-size="15" textLength="100" lengthAdjust="spacingAndGlyphs">
                {word}
            </text>
        </svg>
    }
}

#[derive(Properties, PartialEq)]
pub(in crate::browser) struct CopyButtonProps {
    /// Text put on the clipboard.
    pub(in crate::browser) text: AttrValue,
    /// What the toast names as copied: "Copied {subject}".
    pub(in crate::browser) subject: AttrValue,
    #[prop_or_default]
    pub(in crate::browser) class: Classes,
    #[prop_or_default]
    pub(in crate::browser) title: Option<AttrValue>,
    /// Accessible name, when the visible content does not already name it.
    #[prop_or_default]
    pub(in crate::browser) label: Option<AttrValue>,
    #[prop_or_default]
    pub(in crate::browser) children: Html,
}

/// A button that copies text and reports the outcome through the root toast.
#[function_component(CopyButton)]
pub(in crate::browser) fn copy_button(props: &CopyButtonProps) -> Html {
    let toaster = crate::browser::toast::use_toaster();
    let onclick = {
        let text = props.text.clone();
        let subject = props.subject.clone();
        Callback::from(move |_| {
            let toaster = toaster.clone();
            let subject = subject.clone();
            crate::browser::clipboard::copy_text(&text, move |result| {
                toaster.settled(match result {
                    Ok(()) => format!("Copied {subject}"),
                    Err(error) => format!("Copy failed: {error}"),
                });
            });
        })
    };
    html! {
        <button
            class={props.class.clone()} type="button"
            title={props.title.clone()} aria-label={props.label.clone()}
            {onclick}
        >{props.children.clone()}</button>
    }
}
