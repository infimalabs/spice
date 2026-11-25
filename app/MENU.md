# Menus

The left composer/principal overlay, the right Spice menu, Help sheet, team arrangement, and each lane's menu.

## Keyboard help

A bare `?` outside text entry, Escape with nothing left to dismiss, the global
composer's **Help** tab, or the ⓘ in its totals badge opens one centered modal
help sheet over the shared frosted veil. The sheet and veil fade like the
sidebar. The control stays live while a submission is pending and opens Help
above an open composer.
Escape, **Close**, or the veil dismiss only the sheet, and focus returns to its
opener while that opener can still take it. Repeated separate Escape presses
toggle Help at the top level; held-key repeats and IME composition do not.
An active sidebar, menu, composer or native modal takes Escape first, without
opening Help on the same event. `?` typed into a field, pressed with Command,
Control or Alt, or pressed over another native modal does nothing, so the image
viewer and annotation keep their keys. The sheet links to Chat, Work and Spice
and lists the current shortcuts by area. `SECTIONS` in `src/browser/help.rs` is
the shortcut inventory: a change to any shortcut handler updates its row there.

The shared glass switch selects **Help** or **Documentation**. Every opening
starts on Help without a read. Documentation lazily reads the resident's linked
rack: directories, breadcrumbs and its own Back/Forward stack stay inside the
sheet and never change the app URL or browser history. Relative and
`spice://docs` links resolve lexically inside the docs root. HTTP/HTTPS links
open an isolated new tab; other schemes and escaping paths are inert. Markdown
uses the shared sanitized renderer, diagrams and copy controls; images render
only from admitted raster replies.

Page reads discover the current rack identity; cached bytes belong to its exact
archive digest, optional admission code and normalized path. Connection changes
and new identities invalidate old bytes. Request sequencing rejects late replies.
Loading exposes `aria-busy`; refusals retain the last accepted page, report the
resident's error and offer Retry.

In the managed browser, toggle Help with separate Escape presses at the top
level, including from an inline text field without changing its contents.
Check that Escape first dismisses an open sidebar, menu, composer or image
viewer without opening Help on that event. Open the sheet with `?` and with
the control, including over an open sidebar; Escape, **Close** and the veil
must close only the sheet.
Check focus return, each place link, and light, dark, desktop and 390px phone
layouts without horizontal overflow or console errors.
Also check lazy Documentation selection, directory and Markdown links, admitted
images, local Back/Forward, a delayed read superseded by navigation, refusal and
Retry. Reopening must return to Help. A development executable with no linked
rack reports that refusal; disposable response fixtures can exercise the reader
without adding documentation bytes to the product.

## Message action drawers

Opening a message-card action drawer leaves the card box fixed and uses the
shared veil frost to defocus everything below its sharp control bars. Existing
structural ordering keeps every drawer rail on that card unobstructed above the
veil. Switching directly to another drawer keeps the veil continuously in place
without changing drawer stacking. While a drawer is open, incoming cards do not
move its card even at the stream's live edge. Choosing any action or moving
focus outside closes the drawer; moving focus between its controls does not.

Actions stay at their final open positions throughout opening and closing.
Only the opener and reveal edge move, so an early tap reaches the same action.

Sent-image attachments and full-card tool images use the same sharp control
plane above the frost. Their image drawers support Escape, focus-exit dismissal,
and both Quote destinations without opening the image underneath.

## Teams in the global menu

The Spice view leads with connection state, the inspectable version stack, and
the theme selector. Global TURBO stands beside the Spice button in its header,
starting where the theme selector starts, with the weekly percentage above the
button. A superseded app generation keeps running until its session
fails against the resident serving the replacement; that failure reloads the
tab into the served generation once per generation, unless a composer draft or
unsettled submission would be lost, in which case the tab stays and reconnects
behind the reload control.

The global Spice summary stands in the open Spice view's header, which keeps
one fixed height. Its title reads `spice` in lowercase; ordinary sidebar and
sheet kickers retain uppercase. The bare product version sits on the title's
baseline beside it, since the title already names the product; a header too
narrow for it hides the plain version. Beneath them the current connection
state is the header's status line: it runs under the theme selector and TURBO
and ends in an ellipsis before the weekly percentage. An available update reads
as the version in the focus accent with a reload mark and stays actionable:
where the header is too narrow for the version beside the title, on a phone or
beside the Windows caption buttons, the update takes the status line's place
while the state stays announced. The admitted journal stream cursor is in the
connection state's hover title and accessible name, and only while the resident
connection is current; startup and reconnect states never present a retained
runtime's cursor as live.

The version's hover title and accessible name carry the full identity, and an
update's carry the same rows beside the generation it offers. Three of its
numbers count different things and are never compared. The crossing wire
generation is the outer envelope every request and frame carries; a build
speaks exactly one and admits no other, so it stays at its one value while
the others advance. The journal migration schema counts the stored journal's
migrations, the only schema of the three. The browser app protocol counts
changes to the browser/resident contract, which a loaded page must match to
connect.

After **Principals**, **Teams** is an interactive team roster. Each team lists
its agents with presence, name, worktree, model, pending steering, liveness,
held task, kept jobs, and provider emblem; an agent opens its composer in Chat.

Formed teams stand in shared order, each named for member zero, followed by
**Create new team** and **Unassigned Agents**. A mouse drags the whole tile or
card; a finger or pen drags a tile by its name or a card by its header,
leaving the rest available for scrolling. Holding a lift near the sheet's
edge scrolls it. A member dropped on an independent team or its member joins
it; on a member of its own team reorders it; on Unassigned Agents returns to
its own lane-ID team. Another agent's own-ID team cannot be joined.
Dropping an agent on Create new team starts a fresh independent one-agent
formation, including for an unassigned agent. That formation survives
arrangement and can accept repeated joins without changing identity.
A team keeps its identity whoever leaves and dissolves when a departure leaves one member,
who returns to the team named for itself. Dropping a card on another card
reorders teams through guarded `team::arrange`; other changes move one member.
No-op releases send nothing. Teams stop at six members: full teams refuse
joins but allow reordering and departure. This is not a total-agent or
team-count cap. Every change witnesses the revision the roster displayed;
fresh creation also names the agent's observed source team, and the resident
allocates its new identity.

An own-ID singleton defaults to the existing browser-local hidden state.
Returning to it hides it again. Own teams remain listed under Unassigned
Agents even when revealed; their lane links show the same team and focus its
composer. **Hide team** also works for a revealed own team and survives reload.
Hiding a focused composer releases its permalink rather than immediately
revealing it again. Visibility never changes durable membership.

## Who the app names

The app names principals from their profiles, never Git. The connected
principal's own Principals row and inbox heading show their name, initials, and
Gravatar.
Activity, task steps, room lines, and quote bylines use the author's profile;
a quote shows initials with the full name as its accessible label.

Before subscribing, the app reads the connected-readable directory and uses
the caller's roles to select operator-only streams and reads. Any `principal::`
fact re-reads it: profile edits publish in place; role edits reconnect so reads
and subjects change together. A profile without a name reads as
`principal <id>`.

## Principals in the left overlay

The expanded left composer overlay follows the composer with **Principals**.
The connected principal appears there once, as their own row marked `(you)`,
and that row seats their personal inbox with its heading, open count, items,
and controls. The right Spice view never duplicates this human-facing section.
The row and inbox stand whether or not this connection may list principals,
and while the listing loads or is refused; until the principal directory
arrives the row reads as loading. An operator-role principal also sees every
principal the repository has admitted, oldest first. Each reads with its avatar
and initials, name, email, roles, capability standing (issued, local for the
earliest-born, whose capability each resident generation mints, or revoked),
and admission age with the exact instant on hover. Its controls send the
commands `spice principal` sends: **Edit profile** opens a form whose changed
fields replace the recorded ones, **Grant operator** or **Revoke operator**
changes the one role, and **Revoke capability** asks once more before retiring
an issued capability. **Admit a principal**, at the heading's trailing end once the
listing arrives, opens beneath the heading a form taking a required name, an
optional email, and the operator role; the reply's capability shows once above
the list as the opening link `ORIGIN/#CAPABILITY` with **Copy link** and
**Done**, held only in that panel and dropped with its connection. The listing
re-reads on every `principal::` fact, so another session's change shows at
once. The listing is operator-only: a principal without the operator role sees
only their own row and its inbox. When the resident refuses a listing, the
message beneath that row says that the operator role is required. One change is
in flight at a time; its progress and any refusal, such as revoking the last
operator, read under the heading.

## Jobs in the global menu

After **Teams**, the Spice view closes its operational sections with **Jobs**,
every job every lane keeps, apart from the Teams roster. The sections read
**Providers**, **Teams**, then **Jobs**. The list and its
inspection card share one
fixed four-row slot in every state; longer content scrolls inside it. At either
edge, arrow and page keys continue through the containing Spice menu instead of
trapping keyboard navigation in Jobs.

Rows show kind (▶ command, ◆ service, ● watch, ◈ watched service), agent accent
and name, observed start age, and the complete command on hover; unpublished
starts show `?`, and ended jobs awaiting delivery use hollow glyphs. Search
covers agent, command, and job key. Heading counts filter by kind or unheard
endings; **Jobs** clears filters.

Inspection shows command, working directory, keeper lifecycle, start, deadline,
ending evidence, and bounded output tail with an omission notice. **Summary**
and **Output** share the card; previous/next traverse matching jobs. Copy takes
the complete command or displayed output. Back returns to the list; Escape
first cancels a kill confirmation.

**Kill** requires running/starting evidence and current operator authority.
Confirmation names the exact job key, focuses **Cancel**, and asks the keeper
to stop that process group. Stopping, ended, unavailable, and absent jobs
cannot be killed. A stop is not replayed after reconnect, and neither
inspection nor stopping marks the agent's result delivered; an
operator-stopped ending still receives its ordinary agent notification.

The operator-only `lane::jobs-read`, `lane::job-read`, and `lane::job-stop`
operate on keeper records, not a second store. References carry the native
base52 job key plus encoded topology revision/lane slot. Topology movement
invalidates a reference before any stop marker; return to the list for a
current one. Catalogs refresh while shown, live inspection refreshes while
open, and disconnection or missing records is explicit.

## Providers in the global menu

The Spice view's operational sections begin with **Providers**, which
summarizes provider quota and availability. Its **Accounts** subsection groups
the account cards.

**Accounts lacking usage** and **Disabled credentials** appear only when they
have entries. Each account row shows the limit name, a capacity bar
filled with what remains, the amount — `N% left` for a percent limit, `R / L
unit left` for a counted one — and the reset cadence. An account sharing the
weekly limit wears the same accent as its layer in the Spice button's quota
fill; others stay neutral.

Reset-credit summaries retain the provider's available and redeemable totals.
Every reported credit remains listed in provider order; a credit that cannot
currently be redeemed is marked **not redeemable** rather than hidden.

Each active account card's header ends with the report's relative age. Quota
cards only report availability; they carry no control over it. A provider
listed in OMP's `disabledProviders` collapses to one muted placeholder, even
when a retained sample still carries its account reports or OMP omits them.
Enabling or disabling a provider, enrolling an account, and removing a
credential all stay in OMP.

A failed provider-policy read does not discard a successful usage sample:
the cards keep last-known availability, identify the typed policy issue
separately, and continue to report usage freshness. A later successful policy
read replaces that retained projection and clears the issue.

The reset-credit summary states available and redeemable counts, the available
count standing in when the provider omits redeemable. Each credit is one compact
row: its grant age at the left and any valid expiry countdown at the right.
Provider title or program and remaining count stay in the grant label's hover
text; exact grant and expiry times are also available on hover. A
provider-reported raw credit balance shows
on its own account as whole credits, truncated so a fraction never reads as
more than can be spent; it is neither a time-window meter nor a reset grant,
accounts never share or aggregate it, and Spice never infers it.

## Arranging teams

A team is named by its lead agent, canonical member zero, with the label its
messages carry: the configured name, else the worktree leaf. Only a team with
no current members keeps its short id.

Each team console takes its whole base from its age among the teams: teams
count up from the palette's start by when each became a team, so the oldest
wears green, the next blue, then gold, and an unteamed agent takes no place. The
lead wears that base and each later member the next slot up. Colours follow only
that age, so swapping two teams, or opening, closing, or reordering consoles in
one browser, changes none; only a team forming or an older one ending moves a
colour.

Team consoles follow the composer model: the first team in arrangement order
holds the right edge beside the app's controls and each later team, including
one newly formed, stacks to the left of its predecessor; a fleet wider than the
window shows that right edge first. Inside every console the toolbar,
composers, and messages read left to right.

Pick up a team from its pip matrix: dropping on the middle of another team
swaps the two teams' positions; dropping on either outer edge joins the source
into that team, which keeps its identity, primary composer, and member order and
appends the source members in order. The pip stack and the team menu open the
same **Swap positions** and **Join into** choices by keyboard or tap, and the
menu's **Disband team** leaves every member as an unassigned lone seat. Escape
returns focus to whichever opened the menu. Joins stop at six members: a target
that would make a seventh is shown unavailable and sends nothing. Every
accepted arrangement, including disbanding, sends the complete topology at the
drawn revision. **Disband team** toasts its typed refusal and refreshes that
topology; cancellation or a removed source sends nothing.

## Lane menu

Each Chat lane's compact menu button shows that lane's provider emblem, or
omp's pi when the model names no recognized provider, and its hover title
names the model, effort, and fast mode the emblem stands for. It swaps that
lane's composer for its seat panel. The panel leads with what the
agent needs from the operator — consent requests to Allow or Deny in place,
held tasks iced, blocked, or in review, Designs awaiting a decision, a lease
near expiry, and direction left waiting — then the Wake, Drain, Remove, Renew,
and next-launch model, effort, and TURBO commands and their refusals. Renew
submits at once; Drain and Remove confirm first. Effort and TURBO show the
running value beside the next launch. Agent TURBO uses the same control as its
team and Spice parents; either parent forces it on and read-only without
overwriting its own saved selection. Fast mode follows
[the lane loop](../docs/loops.md#l-lane--the-place). Presence,
lease age, current task, latest activity, waiting direction, the lane identity,
occupant, session, and worktree follow, beside **Create own team** and
**Leave all teams**. Creating a team groups the agent under a fresh independent
identity; leaving returns it to its own team and hides that team locally.
If other occupants already hold the returning agent's own ID, the resident
moves those occupants together to a fresh formation and returns the owner
alone in the same guarded transaction. The
button, Escape, or a click elsewhere in the team restores the
composer.
