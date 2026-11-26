# Browser checks

## App checks use OMP's managed Chromium

Follow the repository's [managed-browser procedure](../AGENTS.md#browser-verification):
start `spice dev xtask -- app-demo`, wait for its endpoint, and open it with
OMP eval's `browser`. The demo's `sources` and `staged_generation` must identify
the current app build. Managed Chromium remains headless. For an intentional
theme matrix, call `await tab.emulate({ colorScheme })`; a stored
`spice:app-theme` preference still wins.

## Navigation

Exercise the canonical task, selection, scope, sub-view and graph-option
addresses defined by [Task overview](OVERVIEW.md#task-overview), plus Spice at
`#/spice`. Graph node budget stays local.

Click destinations and native card links: each adds one history entry; clicking
the current destination adds none. Scope chips, filter and graph adjustments
replace. Back, Forward, reload and a copied URL must restore the same complete
selection and options. Keep an unsent composition throughout navigation.
Close, Escape and backdrop unwind an owned panel sequence to its Chat opener.
In a fresh tab or manually entered panel address, they replace with Chat and
never leave the app. Card-link Back restores the previous panel; reopening
Spice makes the same lane link a fresh navigation and preserves focus arrival.

Inspect `history.state.spiceNavigation` after navigating, composing and opening
a Principals form. It must preserve other owners' fields and restore each
entry's Work row, panel offset, Chat top-card offsets, graph zoom/pan/budget,
Jobs selection/filter/query and focus key. Never store drafts, attachments,
requests, results, capability links, residency witnesses or Principals forms.
An unknown presentation version must not prevent URL navigation.

Exercise delayed rendering: restoration waits about 1.5 seconds, but a newer
arrival or reader scroll, pointer or key cancels it. Missing rows open nothing;
missing cards leave their plane unchanged; panel offsets clamp after the wait.
Jobs reopen only when the current resident generation and catalog still match.

Scrolling and focus write that presentation at most once a second, and only
when it changed. A link the reader follows records the departing place at
once. WebKit refuses more than 100 history writes in 30 seconds, so sustained
scrolling must never approach that budget. A refused presentation write is
dropped and the app keeps running. To check, count `replaceState` calls
during a long scroll.

Check at desktop and phone widths, including reduced motion. Pending detail
must hold its address; only settled absence removes missing selection roots,
with a notice, never widening the selection. A missing task opens Work; no
remaining roots or malformed addresses open Chat. Invalid sub-views return to
the task room. Authentication normalization replaces rather than pushes.

## Keyboard help

Toggle the [help sheet](MENU.md#keyboard-help) with separate Escape presses at
the top level, including from an inline text field without altering its
contents. Escape must first dismiss any sidebar, menu, composer or image viewer,
without opening Help on that event. Open Help with `?` and with the control,
including over an open sidebar; Escape, **Close** and the veil close only Help.
Check focus return, each place link, held-key/IME suppression, and light, dark,
desktop and 390px phone layouts without horizontal overflow or console errors.

## Pass decisions

A Design whose completed pass offers outputs opens in its **Yours** row.
Check per-output Accept/Reject, optional output comments, bulk verdicts,
offered-output acceptance, the pass fact and tally, and the single
overall comment. Return requires non-whitespace text; the resolved-decision
button requires every verdict and names the result it will record.

Exercise actual decisions and returns on disposable app-demo passes at
1440px and 430px in Light and Dark. Verify the journal outcomes, including
individual verdicts and comments, rather than treating a socket reply as a
committed board transition. While sending, fields lock under a dashed notice
and the row stays put; a refusal retains the draft. At phone width the action
bar is fixed to the viewport bottom without horizontal overflow.

Keep process-based fixtures and wire-delay probes disposable. A moved native
witness can prove refusal retention without inventing a successful response.

## Design decision history

Open the concluded `DEMO-dPCb7200m` Design and activate **History**. Its permanent
demo decision `1` names its journal fact code, retains
the operator comments, and shows accepted output `DEMO-dPCb71zdX` as done at
integrated commit `a53ad58b1`; the completed output must never read as
unavailable merely because it left the open board. An output outside that
window resolves through the mounted Design's bounded detail queue, then shows
the same settled state. Check this view at 1440px and 430px in Light and Dark,
with no horizontal overflow or browser errors.
Decision cards in the room and History share this same fact-code and
output-state presentation.

## Conclusion and collapse decisions

A Design whose completed pass offers no output opens it in its **Yours** row.
Check the pass fact, accepted decision comment, released output commit and
final-review evidence, unfinished controlled work, and the
statement/workflow/claim witnesses. **Conclude as done** is enabled only while
the reason is blank; **Collapse the Design** and **Return to the room** are
enabled only while it has material text.

The action bar and context disclosures are the complete GUI surface. They do
not render a terminal command or serialized decision; the native actions own
the prepared request.

Released output evidence does not widen the board's 20-row recent-finished
window. Every accepted output of the mounted Design enters the existing
bounded task-detail queue by exact identity; a board row supplies immediate
statement/status evidence while detail supplies its complete evidence. If the
identity is outside the board window, the evidence row reads as loading until
that answer lands, names a real read refusal and its diagnostic, and calls the
output missing only after `task::detail-unknown`.
Final-review wording follows the output workflow's positive clean settlement,
independently of the commit boundary: a Git-backed review fact, a finding, or a
pending clean review never reads as a clean final review. A completed clean
review must retain its fact, reviewer, status, and note without Git; an output
with no review must say so rather than imply success.

Exercise accepted and returned decisions as under
[Pass decisions](#pass-decisions), also verifying the conclusion fact.

## Finished work disclosure

The closing Done group starts collapsed in a work scope. Selecting a finished
task directly, including a concluded Design's permalink, reveals its row once.
The Done button can then collapse and reopen that row; filtering or refreshing
the same selection must not override the operator's choice. A different direct
selection reveals its own row, while a new work scope starts collapsed.

Check completed tasks and concluded Designs at 1440px and 430px: initial reveal,
`aria-expanded=false` with no rendered finished rows after collapse, and the
same selected row and history after reopening. Returning to all work preserves
the scoped Done count rather than counting only the previously selected task.

Open a concluded Design's Permalink from an expanded Done row and a fresh
page: both must select its exact task address without an intermediate close.
Cover arrival from Work, Spice and startup before hydration, with normal and
reduced motion; dismissal returns focus to a surviving launcher.

## Mosaic card sizing

Prose cards fit their content and sit vertically centered in their allocated
lattice cells, capped at the cell height. Check the real message structure,
including its `.mosaic-message-content` clipping grid, prose, quotes and controls:
bare text in a card does not exercise the nested grid's intrinsic sizing.
Measurement cards keep natural height; boundary rows and image covers fill their
cells.

## Message action drawers

A message action rail reserves exactly one full-size opener. Closed, it is the
only control in flow and only its upper-left and lower-left corners are rounded;
Play, Copy, Quote into Draft, and Quote are clipped outside layout, pointer, and
focus participation. Opening slides the opener toward the left and reveals every
action as a square button between it and the stationary card edge. Closing
retracts the strip. The rail footprint and card box must not resize. Prose and
provider-failure cards, room publications, composer media, and rendered Markdown
images use this same structure.

At 1440px and 430px, compare the rail and card rectangles before and after
opening. Check that closed drawers expose only the full-size opener, every
action is usable when open, one-open-drawer exclusivity, Escape, keyboard focus,
Play, Copy from the first, middle and final hunks, both Quote destinations,
reduced motion, viewport clipping, and browser errors. Copied quoted lines keep
one `> ` per depth (`>` when blank), and a blank line ends each quote.

## Theme selector material

The Spice sidebar keeps Light, System, and Dark as three pressed buttons in one
theme group. Their selected face slides behind the semantic keyboard controls
and uses the transmitted, lit glass recipe of an active team slider thumb.
Its gradient, edge, and light catches remain legible without backdrop
filtering; reduced motion moves the presentation-only face without travel.

## Task graph workflow cues

Check the [Work standings](OVERVIEW.md#task-overview) and
[Design standing](DESIGNS.md#design-standing) on actual Mermaid overlays.
Each node has one frame, not a diagram stroke beneath a second overlay frame.
Passive and in-flight are solid, waiting is dotted, deleted is dashed, and
done has a quiet rounded tag. Verify waiting's containing-surface tone, the
pass-ready and awaiting-decision marks, and distinctions under reduced
contrast and forced colors; waiting must not borrow warning emphasis.

The graph, its lenses and its controls follow
[Task overview](OVERVIEW.md#task-overview). Check all four independent
Prerequisites/Dependents combinations (no Both button), and Drafts, Controllers
and Settled separately and after URL reload. A single-task address keeps its
own task visible. Filters preserve depth and list rows; lenses retain
truncation, retry, native pan/zoom/fit and keyboard overlays.
Activating a root focuses and opens its existing row. Activating an on-board
neighbor extends the URL's complete ordered selection, including roots hidden
by the filter, and opens the new row; an identity absent from the board stays
visible context.

## Browser branding

The tab title and menu use the lowercase brand `spice`, trailing any changing
state. Once a board snapshot arrives, the title and pepper icon count the
global execution Work rail while it has open work, otherwise the global Design
rail, never their sum: `5 tasks - spice` or `1 design - spice`. The badge is
green for execution and plum for Design, exact through 99, then `99+`; the
title always retains the exact count. When both rails are empty on a current
connection, the icon drops the badge and shows the pepper alone, large and
centred, and the title reads `No open work - spice`.
Its upper-right dot takes the shown rail's work mark first (blocked, decision,
pass, or deferred), otherwise the Spice button's neutral inbox-waiting dot,
and disappears when neither applies. The icon uses the dark palette on its
own dark ground. Reconnect keeps the last kind and count marked stale in amber,
for example `7 designs (stale) - spice`, even when that count is zero;
`Loading - spice` and `Disconnected - spice` retain their distinct badges.
Before the app loads the title is `spice`. Speech never retitles the tab.

## Image interaction

Exercise [image interaction](IMAGES.md) on real composer and retained-message
surfaces.

## Draft composer parity

The global composer is the left overlay's strip under the header. Its circular
operator-avatar launcher mirrors the Spice button's size and browser-edge
position, shows the operator's face edge to edge inside a hairline rim with no
initials band, opens or closes the human side, and carries the personal inbox's
open-count badge. Check that the editor's DOM identity, typed text and
selection survive expansion, dismissal and conversion, and that the strip keeps
the destination tabs and live Draft and Help without a strip-wide divider; the
sheet below is cut off and out of the Tab order.
Choosing the operator launcher or a destination opens the sheet with the editor
focused and fades in the shared page veil; dismissal fades the veil out. Open,
the sheet runs to the viewport's foot: editor, To row with Send/Create, then
Principals and the inbox scrolling on its own. Open Spice beside it on a
desktop viewport: the left sheet and right sidebar both stand above exactly
one veil, with no additional darkening when the second sheet opens. Neither
sheet masks the other, both stay interactive, and closing either leaves the
same veil under the survivor; only closing the last removes it. A press inside
either keeps both, a press on the page beside them closes the left one, and
Escape closes the newest first. Opening the strip while Spice is open must
work the same. On a phone the closed strip shows the operator launcher and
current destination; with both overlays requested, Spice takes the screen and
the left sheet returns when it closes. In both themes and inline directions,
look for a bright hook in either gutter, an inner edge, doubled ring or seam.
Opening must not move or rewrap the already-laid-out editor.
By
keyboard, each agent checkbox announces its name and state, Best-matched is
pressed only with none chosen, and choosing never resizes the dialog. Several
chosen agents each get one card and one toast names every outcome; a refusal
keeps the composition with only refused agents chosen.
A durably accepted best-matched Ask must close the sheet,
open that response's exact primary composer in Chat, reveal a hidden team and
focus its writing field; Back restores the preceding route. Repeat the Ask
while already on that agent's route: focus must still reach its writing field,
without another history entry. Chosen agents or a broadcast must not hand off.
While the response is pending, keep
the route; refusal or transport failure must retain the composition. If the
accepted lane is absent from the current roster, keep the current route under
the accepted-for-agent toast rather than opening a broken target.
Once any submit is locally admitted the dialog is busy: its composition
desaturates and seals against every edit and resubmission, and the
action keeps its word and reads busy, as the team SEND does. Escape or an
outside click still collapses it; the strip stays clickable and reopens the same
sealed pending view. Durable
acceptance alone clears or hands off the submitted content. A
refusal or transport failure restores the exact text, media, recipient and work
fields and toasts the server error, returning focus to the editor only while the
dialog is still open. Every send, creation, save and refusal reports on the
shared toast and fades: compare the composer's and the Draft editor's height
before and after each, and find no status line in either.

Press Command-Enter in Ask mode: one send goes to the chosen or best-matched
agent; in Task mode the first line alone creates nothing, and a blank line and
pitch paragraph create once. Check all To agents, two or more, vacant
included, or hold Command-Shift in a focused Ask with two or more occupied:
SEND compresses into the team's tinted FULL SEND; unchecking or releasing
restores SEND. Press Command-Shift-Enter on an Ask: the dialog goes pending,
each occupied agent gets one authored-body card, and acceptance clears the
composer. Other modes leave composition and board unchanged.

Switch all four kinds with text, quotes and images present; the sheet, editor
and action dimensions must survive every mode change, resizing and media
addition, and Ask's wrapping recipients stay beside the action on each browser.
Quotes stay above the editor. Images use the
team composers' bounded, scrolling thumbnail strip over its lower inline end,
clearing writing space only while present; each thumbnail opens and removes as
in a team composer. Its attachment chooser is touch-only; desktop adds images
by paste or drop. Only Ask adds To and its
subdued Best-matched choice; work changes the textarea's title/pitch/context
template, never adding a Pitch, Topic or second context field.

Compare the destination, DRAFT and HELP tabs with the hanging totals: only
each run's outside lower corners round, and each tab owns its lower border. No
strip-wide divider or accent rail crosses the destination tabs. The pressed
destination, and Draft when on, wear the kind's accent; Draft is inert for
Ask. Ask is neutral; Task, Plan and Design use green, blue and plum in both
themes.

Check the opening/closing fade against Spice: no moving edge or rewrapped
writing, immediate reduced-motion settlement, focus outside the closed strip,
and no Escape reopening during close. Empty/whitespace content
leaves Send/Create muted; applicable content lights the team SEND backlight
without a readiness rim. Hover/focus brighten it; pressing dims it. Held
Command adds the 2px command ring: Ask shares FULL's semantic blue while Task,
Plan and Design retain their kind accent; Ask's Command-Shift ring uses ink.

Send text, then an image alone: each arrives once and clears on acceptance.
Reload unsent text, quotes and an image; all survive. Create work with title,
pitch, description, quote and image; its unplaced task detail retains them.
Draft off stages then files after admission: the transient Draft row and a
submitted Draft left by phase-zero validation remain read-only, while a
transport refusal retains the local composition. Draft on records an editable
Draft and persists across dismissal. Creating the first editable Draft updates
the shared totals strip and `/work/drafts`; its width affects the editor only
below the left overlay's preferred width, without shifting the right overlay
or its content clearance. Check focus containment, image annotation and
narrow-view overflow.

Exercise [Draft editing](OVERVIEW.md#drafts-and-oops-intake) through creation,
reopening after reload, and Save with an annotated image in every Kind.
Verify image artifacts and description in task detail, unchanged-media Save,
whole-set refusal before creation, and Command- or Control-Enter submission.

Every Quote control, on an agent message, a directive hunk, an image, or a
room post, has a Quote into new Draft companion. It adds the same quote shard
under the creator's key instead of the source's own composer, opens the main
composer in Task mode, and focuses that shard; an ordinary Quote still drafts into
its own composer. The creator renders and composes its shards exactly as a lane
send does, so the created Draft's description carries each quote link, with its
reply, before the text in shard order. Task detail presents those links as the
resident resolved them, and the Draft's own editor resumes from the whole
composed description. Check by quoting a chat message into a new Draft,
creating it, and finding the quoted source in its task detail.
Each quote arrival opens the creator only once. After changing its mode,
submission settlement must not replay that arrival: an accepted Message stays
in Message mode, refused Plan/Design work retains its mode and editor focus,
and successful work creation closes normally. A new quote arriving during
admission waits until settlement; it does not lose its one opening.

Exercise the shared [message-card](ROOMS.md#message-cards) and
[composer activity-rail](TRACE.md) contracts on real
transcript cards.
