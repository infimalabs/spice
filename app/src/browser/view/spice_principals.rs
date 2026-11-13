//! The Principals section of the global menu: the connected principal's own
//! row, which seats their personal inbox, and for an operator-role principal
//! every born principal with their initials, code and identity, roles,
//! capability, and admission time, beside the admission, one-time capability
//! handoff, profile edit, operator role, and capability retirement
//! `spice principal` offers, through its commands.

use super::super::clock::{Age, local_full};
use super::chrome::CopyButton;
use super::*;
use crate::client_action::{PrincipalResponse, PrincipalWrite};
use crate::socket::SessionScope;
use wire::principal::{
    PrincipalAdmit, PrincipalCode, PrincipalDeauthorize, PrincipalDirectory, PrincipalEdit,
    PrincipalListing, PrincipalProfile, PrincipalRole, PrincipalRoleRef, PrincipalRoles,
    PrincipalStanding, PrincipalsAnswer,
};

const HEADING: &str = "spice-principals-heading";

#[derive(Properties, PartialEq)]
struct PrincipalsProps {
    /// Every born principal, or why the resident refused this principal the
    /// listing; absent until the first read answers.
    listing: Option<Result<PrincipalsAnswer, AttrValue>>,
    /// Whether this connection may read the listing at all.
    manages: bool,
    directory: Option<PrincipalDirectory>,
    actor: wire::Actor,
    scope: SessionScope,
    online: bool,
    on_action: Callback<ClientAction, bool>,
    /// The connected principal's personal inbox, seated in their own row.
    inbox: Html,
}

/// A capability the resident handed off once, until the operator dismisses it.
#[derive(Clone, PartialEq)]
struct Handoff {
    scope: SessionScope,
    name: String,
    link: AttrValue,
}

#[derive(Clone, PartialEq)]
enum Progress {
    Sending(&'static str),
    Failed(String),
}

/// The one inline form open in the panel.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Open {
    Admit,
    Edit(wire::CanonicalUuid),
    Deauthorize(wire::CanonicalUuid),
}

type OpenState = UseStateHandle<Option<(SessionScope, Open)>>;
type Send = Callback<(&'static str, PrincipalWrite)>;

/// The Principals section with `inbox` seated in the connected principal's
/// row. The inbox stands there whether or not this connection may list
/// principals, and while that listing loads or is refused.
pub(super) fn render(
    connection: &Connection,
    runtime: Option<&AppRuntime>,
    sender: &SenderHandle,
    inbox: Html,
) -> Html {
    let Some(runtime) = runtime else {
        return html! {
            <section
                class="app-sheet-section app-spice-principals"
                aria-labelledby={HEADING}
                data-principals=""
            >
                <div class="section-heading"><h2 id={HEADING}>{"Principals"}</h2></div>
                <ul class="app-spice-principal-list">
                    {render_self(None, None, inbox)}
                </ul>
            </section>
        };
    };
    let manages = runtime.offers(CommandKind::PrincipalRead);
    let listing = runtime.principal_listing().map(|listing| {
        listing
            .cloned()
            .map_err(|reason| AttrValue::from(reason.to_owned()))
    });
    let directory = runtime.principals().cloned();
    let actor = runtime.principal().actor().clone();
    let scope = runtime.principal().scope();
    let online = connection.is_online();
    let sender = sender.clone();
    let on_action = Callback::from(move |action| admit_action(&sender, action));
    html! {
        <Principals
            {listing}
            {manages}
            {directory}
            {actor}
            {scope}
            {online}
            {on_action}
            {inbox}
        />
    }
}

#[function_component(Principals)]
fn principals_view(props: &PrincipalsProps) -> Html {
    let progress = use_state(|| None::<(SessionScope, Progress)>);
    let handoff = use_state(|| None::<Handoff>);
    let open = use_state(|| None::<(SessionScope, Open)>);
    let progress_now = progress
        .as_ref()
        .filter(|(scope, _)| *scope == props.scope)
        .map(|(_, progress)| progress);
    let open_now = open
        .as_ref()
        .filter(|(scope, _)| *scope == props.scope)
        .map(|(_, open)| *open);
    let send = use_send(props, &progress, &handoff, &open);
    let ready = props.online && !matches!(progress_now, Some(Progress::Sending(_)));
    let own_self = || {
        let principal = match &props.actor {
            wire::Actor::Principal(principal) => Some(*principal),
            _ => None,
        };
        let identity = props
            .directory
            .as_ref()
            .and_then(|directory| directory.identity(&props.actor).cloned());
        render_self(principal, identity.as_ref(), props.inbox.clone())
    };
    let (rows, note, admit) = match props.listing.as_ref().filter(|_| props.manages) {
        Some(Ok(answer)) => {
            let roles =
                PrincipalRoles::new(answer.principals.iter().map(|listing| listing.head.clone()));
            let own = roles.head(&props.actor).map(|head| head.principal);
            let bootstrap = answer
                .principals
                .first()
                .map(|listing| listing.head.principal);
            let row = |listing: &PrincipalListing| {
                let own = own == Some(listing.head.principal);
                render_principal(&PrincipalRow {
                    listing,
                    directory: props.directory.as_ref(),
                    own,
                    bootstrap: bootstrap == Some(listing.head.principal),
                    open: open_now,
                    ready,
                    scope: props.scope,
                    set_open: &open,
                    send: &send,
                    inbox: own.then_some(&props.inbox),
                })
            };
            let mut rows = answer.principals.iter().map(row).collect::<Vec<_>>();
            if own.is_none() {
                rows.insert(0, own_self());
            }
            let admit = render_admit(
                open_now == Some(Open::Admit),
                ready,
                props.scope,
                &open,
                &send,
            );
            (rows, Html::default(), admit)
        }
        unlisted => {
            let note = match unlisted {
                None if props.manages => {
                    html! { <p class="app-empty-state">{"Loading principals…"}</p> }
                }
                Some(Err(reason)) => html! {
                    <p class="app-empty-state" data-principals-refused={reason.clone()}>
                        {"Managing principals takes the operator role."}
                    </p>
                },
                _ => Html::default(),
            };
            (vec![own_self()], note, Default::default())
        }
    };
    let status = progress_now.map(|progress| match progress {
        Progress::Sending(label) => html! {
            <p class="app-spice-principal-status" role="status">{format!("{label}…")}</p>
        },
        Progress::Failed(error) => html! {
            <p class="app-spice-principal-status" role="alert" data-state="failed">
                {error.clone()}
            </p>
        },
    });
    let handoff_now = handoff
        .as_ref()
        .filter(|shown| shown.scope == props.scope)
        .map(|shown| render_handoff(shown, &handoff));
    let (admit_control, admit_form) = admit;
    html! {
        <section
            class="app-sheet-section app-spice-principals"
            aria-labelledby={HEADING}
            data-principals=""
        >
            <div class="section-heading">
                <h2 id={HEADING}>{"Principals"}</h2>
                {admit_control}
            </div>
            {admit_form}
            {handoff_now}
            {status}
            <ul class="app-spice-principal-list">{for rows}</ul>
            {note}
        </section>
    }
}

/// The connected principal's own row before, or without, the managed
/// listing: avatar, name, and email from the principal directory, which reads
/// as loading until it arrives, then the personal inbox. Its key and child
/// slots match the managed row's, so the listing's arrival keeps the inbox.
fn render_self(
    principal: Option<wire::CanonicalUuid>,
    identity: Option<&wire::principal::PrincipalDisplay>,
    inbox: Html,
) -> Html {
    let name = match (identity, principal) {
        (None, _) => "Loading…".to_owned(),
        (Some(identity), Some(principal)) => display_name(principal, identity.name.as_deref()),
        (Some(identity), None) => identity.name.clone().unwrap_or_else(|| "You".to_owned()),
    };
    html! {
        <li
            class="app-spice-principal"
            key={principal.map_or_else(|| "self".to_owned(), |principal| principal.to_string())}
            data-principal={principal.map(|principal| principal.to_string())}
            data-own="true"
            data-state={identity.is_none().then_some("loading")}
        >
            <div class="app-spice-principal-identity">
                {super::render_operator_avatar(identity)}
                <div class="app-spice-principal-facts">
                    <strong title={format!("{name} (you)")}>
                        {name}
                        <span class="app-spice-principal-own">{" (you)"}</span>
                    </strong>
                    if let Some(email) = identity.and_then(|identity| identity.email.as_deref()) {
                        <span title={email.to_owned()}>{email.to_owned()}</span>
                    }
                </div>
            </div>
            {Html::default()}
            {Html::default()}
            {inbox}
        </li>
    }
}

/// Send one write, reporting its progress in the panel and holding an
/// admission's capability for its one handoff.
#[hook]
fn use_send(
    props: &PrincipalsProps,
    progress: &UseStateHandle<Option<(SessionScope, Progress)>>,
    handoff: &UseStateHandle<Option<Handoff>>,
    open: &OpenState,
) -> Send {
    let progress = progress.clone();
    let handoff = handoff.clone();
    let open = open.clone();
    let on_action = props.on_action.clone();
    let scope = props.scope;
    Callback::from(move |(label, request): (&'static str, PrincipalWrite)| {
        progress.set(Some((scope, Progress::Sending(label))));
        open.set(None);
        let respond: PrincipalResponse = {
            let progress = progress.clone();
            let handoff = handoff.clone();
            std::rc::Rc::new(
                move |result: Result<PrincipalStanding, String>| match result {
                    Ok(standing) => {
                        progress.set(None);
                        if let Some(capability) = standing.capability {
                            handoff.set(Some(Handoff {
                                scope,
                                name: display_name(
                                    standing.principal,
                                    standing.profile.name.as_deref(),
                                ),
                                link: opening_link(capability).into(),
                            }));
                        }
                    }
                    Err(error) => progress.set(Some((scope, Progress::Failed(error)))),
                },
            )
        };
        if !on_action.emit(ClientAction::Principal { request, respond }) {
            progress.set(Some((
                scope,
                Progress::Failed("The app could not queue this change.".to_owned()),
            )));
        }
    })
}

fn render_handoff(shown: &Handoff, handoff: &UseStateHandle<Option<Handoff>>) -> Html {
    let dismiss = {
        let handoff = handoff.clone();
        Callback::from(move |_: MouseEvent| handoff.set(None))
    };
    html! {
        <div class="app-spice-principal-handoff" role="status">
            <p>
                {format!(
                    "Give {} this link to connect as themself. It is shown once: \
                     the journal keeps only its digest.",
                    shown.name
                )}
            </p>
            <code data-principal-opening-link="">{shown.link.clone()}</code>
            <div class="app-control-rail">
                <CopyButton text={shown.link.clone()} subject="opening link">
                    {"Copy link"}
                </CopyButton>
                <button type="button" onclick={dismiss}>{"Done"}</button>
            </div>
        </div>
    }
}

struct PrincipalRow<'a> {
    listing: &'a PrincipalListing,
    directory: Option<&'a PrincipalDirectory>,
    own: bool,
    bootstrap: bool,
    open: Option<Open>,
    ready: bool,
    scope: SessionScope,
    set_open: &'a OpenState,
    send: &'a Send,
    /// The personal inbox the connected principal's own row seats.
    inbox: Option<&'a Html>,
}

impl PrincipalRow<'_> {
    /// Open `target`, or close it when it is already open.
    fn toggle(&self, target: Open) -> Callback<MouseEvent> {
        let set_open = self.set_open.clone();
        let scope = self.scope;
        let opened = self.open == Some(target);
        Callback::from(move |_: MouseEvent| set_open.set((!opened).then_some((scope, target))))
    }
}

fn render_principal(row: &PrincipalRow<'_>) -> Html {
    let head = &row.listing.head;
    let principal = head.principal;
    let code = PrincipalCode::of(head);
    let name = head
        .profile
        .name
        .clone()
        .unwrap_or_else(|| code.to_string());
    let mut identity = row
        .directory
        .and_then(|directory| directory.identity(&wire::Actor::Principal(principal)))
        .cloned()
        .unwrap_or_else(|| head.profile.display(principal, code));
    identity.name.get_or_insert_with(|| name.clone());
    let operator = head.holds(PrincipalRole::Operator);
    let capability = match (&head.capability, row.bootstrap) {
        (Some(_), _) => "capability issued",
        (None, true) => "local capability",
        (None, false) => "capability revoked",
    };
    let admitted = &row.listing.admitted;
    let admitted_local = local_full(admitted);
    let role_label = if operator { "operator" } else { "no role" };
    let role = {
        let send = row.send.clone();
        let reference = PrincipalRoleRef {
            principal,
            role: PrincipalRole::Operator,
        };
        Callback::from(move |_: MouseEvent| {
            send.emit(if operator {
                (
                    "Revoking the operator role",
                    PrincipalWrite::Revoke(reference.clone()),
                )
            } else {
                (
                    "Granting the operator role",
                    PrincipalWrite::Grant(reference.clone()),
                )
            });
        })
    };
    let form = match row.open {
        Some(Open::Edit(open)) if open == principal => render_edit(row, &head.profile),
        Some(Open::Deauthorize(open)) if open == principal => render_deauthorize(row, &name),
        _ => Html::default(),
    };
    let name_title = if row.own {
        format!("{name} (you)")
    } else {
        name.clone()
    };
    html! {
        <li
            class="app-spice-principal"
            key={principal.to_string()}
            data-principal={principal.to_string()}
            data-own={row.own.then_some("true")}
        >
            <div class="app-spice-principal-identity">
                {super::render_operator_avatar(Some(&identity))}
                <div class="app-spice-principal-facts">
                    <strong title={name_title}>
                        {name}
                        if row.own {
                            <span class="app-spice-principal-own">{" (you)"}</span>
                        }
                    </strong>
                    <span
                        data-principal-code={code.to_string()}
                        title={format!("code {code} · {principal}")}
                    >
                        {"code "}
                        <code>{code.to_string()}</code>
                        {format!(" · {principal}")}
                    </span>
                    if let Some(email) = head.profile.email.as_deref() {
                        <span title={email.to_owned()}>{email.to_owned()}</span>
                    }
                    <span
                        data-principal-roles={if operator { "operator" } else { "" }}
                        title={format!("{role_label} · {capability} · admitted {admitted_local}")}
                    >
                        {role_label}
                        {" · "}
                        {capability}
                        {" · admitted "}
                        <Age
                            authored={admitted.get().as_millisecond()}
                            class={classes!("app-spice-principal-admitted")}
                            title={admitted_local}
                        />
                    </span>
                </div>
            </div>
            <div class="app-control-rail">
                <button
                    type="button"
                    disabled={!row.ready}
                    onclick={row.toggle(Open::Edit(principal))}
                >{"Edit profile"}</button>
                <button type="button" disabled={!row.ready} onclick={role}>
                    {if operator { "Revoke operator" } else { "Grant operator" }}
                </button>
                if head.capability.is_some() {
                    <button
                        type="button"
                        disabled={!row.ready}
                        onclick={row.toggle(Open::Deauthorize(principal))}
                    >{"Revoke capability"}</button>
                }
            </div>
            {form}
            {row.inbox.cloned().unwrap_or_default()}
        </li>
    }
}

fn render_edit(row: &PrincipalRow<'_>, profile: &PrincipalProfile) -> Html {
    let name_ref = NodeRef::default();
    let email_ref = NodeRef::default();
    let principal = row.listing.head.principal;
    let onsubmit = {
        let send = row.send.clone();
        let set_open = row.set_open.clone();
        let (name_ref, email_ref) = (name_ref.clone(), email_ref.clone());
        let current = profile.clone();
        Callback::from(move |event: SubmitEvent| {
            event.prevent_default();
            let changed = |input: &NodeRef, current: Option<&String>| {
                field(input).filter(|value| Some(value) != current)
            };
            let edit = PrincipalEdit {
                principal,
                name: changed(&name_ref, current.name.as_ref()),
                email: changed(&email_ref, current.email.as_ref()),
            };
            if edit.name.is_none() && edit.email.is_none() {
                set_open.set(None);
                return;
            }
            send.emit(("Saving the profile", PrincipalWrite::Edit(edit)));
        })
    };
    html! {
        <form class="app-spice-principal-form" data-principal-edit="" {onsubmit}>
            <label>
                <span>{"Name"}</span>
                <input
                    ref={name_ref}
                    type="text"
                    value={profile.name.clone().unwrap_or_default()}
                />
            </label>
            <label>
                <span>{"Email"}</span>
                <input
                    ref={email_ref}
                    type="email"
                    value={profile.email.clone().unwrap_or_default()}
                />
            </label>
            <div class="app-control-rail">
                <button type="submit" disabled={!row.ready}>{"Save profile"}</button>
            </div>
        </form>
    }
}

fn render_deauthorize(row: &PrincipalRow<'_>, name: &str) -> Html {
    let principal = row.listing.head.principal;
    let confirm = {
        let send = row.send.clone();
        Callback::from(move |_: MouseEvent| {
            send.emit((
                "Revoking the capability",
                PrincipalWrite::Deauthorize(PrincipalDeauthorize { principal }),
            ));
        })
    };
    html! {
        <div class="app-spice-principal-form" role="group">
            <p>
                {format!(
                    "{name} loses this capability and every session it opened. \
                     Their history keeps their name."
                )}
            </p>
            <div class="app-control-rail">
                <button
                    type="button"
                    data-state="danger"
                    disabled={!row.ready}
                    onclick={confirm}
                >{"Revoke capability"}</button>
            </div>
        </div>
    }
}

/// The Admit a principal toggle for the Principals heading, and the admission
/// form that stands beneath the heading while it is open.
fn render_admit(
    open: bool,
    ready: bool,
    scope: SessionScope,
    set_open: &OpenState,
    send: &Send,
) -> (Html, Html) {
    let toggle = {
        let set_open = set_open.clone();
        Callback::from(move |_: MouseEvent| set_open.set((!open).then_some((scope, Open::Admit))))
    };
    let control = html! {
        <div class="app-control-rail">
            <button
                type="button"
                aria-expanded={if open { "true" } else { "false" }}
                disabled={!ready}
                onclick={toggle.clone()}
            >{"Admit a principal"}</button>
        </div>
    };
    if !open {
        return (control, Html::default());
    }
    let name_ref = NodeRef::default();
    let email_ref = NodeRef::default();
    let operator_ref = NodeRef::default();
    let onsubmit = {
        let send = send.clone();
        let (name_ref, email_ref, operator_ref) =
            (name_ref.clone(), email_ref.clone(), operator_ref.clone());
        Callback::from(move |event: SubmitEvent| {
            event.prevent_default();
            let operator = operator_ref
                .cast::<web_sys::HtmlInputElement>()
                .is_some_and(|input| input.checked());
            let admit = PrincipalAdmit {
                name: field(&name_ref),
                email: field(&email_ref),
                roles: operator
                    .then_some(PrincipalRole::Operator)
                    .into_iter()
                    .collect(),
            };
            // An admission naming no one would copy this repository's own Git
            // identity, which belongs to the principal already admitted.
            if admit.name.is_none() {
                return;
            }
            send.emit(("Admitting", PrincipalWrite::Admit(admit)));
        })
    };
    let form = html! {
        <form class="app-spice-principal-form" data-principal-admit="" {onsubmit}>
            <label>
                <span>{"Name"}</span>
                <input ref={name_ref} type="text" required=true autocomplete="off" />
            </label>
            <label>
                <span>{"Email"}</span>
                <input ref={email_ref} type="email" autocomplete="off" />
            </label>
            <label class="app-spice-principal-check">
                <input ref={operator_ref} type="checkbox" />
                <span>{"Operator role"}</span>
            </label>
            <div class="app-control-rail">
                <button type="submit" disabled={!ready}>{"Admit"}</button>
                <button type="button" onclick={toggle}>{"Cancel"}</button>
            </div>
        </form>
    };
    (control, form)
}

/// A trimmed, non-empty input value.
fn field(input: &NodeRef) -> Option<String> {
    let value = input.cast::<web_sys::HtmlInputElement>()?.value();
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// How a principal reads where its profile records no name.
fn display_name(principal: wire::CanonicalUuid, name: Option<&str>) -> String {
    name.map_or_else(|| format!("principal {principal}"), str::to_owned)
}

/// The app opening that connects as the principal `capability` authenticates.
fn opening_link(capability: wire::CanonicalUuid) -> String {
    let origin = web_sys::window()
        .and_then(|window| window.location().origin().ok())
        .unwrap_or_default();
    format!("{origin}/#{capability}")
}
