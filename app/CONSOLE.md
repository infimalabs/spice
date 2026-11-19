# Always-chat team console

Every team surface, populated or empty, keeps its Chat composer and message
plane mounted; there is no Work/Chat mode or mode-owned scroll state. Chat uses
the canonical base location `#/chat`, extended by `/card/{target}` for a
retained card. [Work and Spice navigation](OVERVIEW.md#app-sidebar) keeps Chat
mounted; that contract owns address restoration, history and panel dismissal.

Task overview rows show the resident's ownership below the phase: an owner-team
count, Reserved, or No owner. Hover names the exact team IDs. A row read
without ownership shows no inferred owner. Team policy and membership
facts refresh the board as well as the affected lane surfaces.
The existing connection probe also observes the resident repository's config
revision on a fixed cadence, even while other traffic arrives. Its first or
changed revision refreshes ownership; an unchanged revision causes no board
read or redraw. Invalid configuration surfaces the capture fault through the
connection error rather than silently presenting stale ownership as current.

Consoles of every width, height, and membership keep a usable header,
composer, and stream without a nested vertical scroller. The
[toolbar seam](#toolbar) continues from the task band to the consoles' bottom.

Console times never read in UTC unless UTC is the viewer's zone. Live text is
relative, advancing on its own deadline, with full local time on hover;
static renderings such as a task overview's Mermaid labels use compact local
time, so their source and image stay fixed. Canonical UTC
instants appear only in machine-readable attributes and payloads.

Console positions follow the
[shared condensed-sequence contract](../docs/interfaces.md#the-json-outcome).
The header alone leads with the resident-reported integer cursor, followed by
its code in parentheses.

## Jobs

The Spice menu shows all retained keeper jobs. Task and Design overviews reuse
its four-row scrolling list, search, kind and unheard filters, and inspector,
but include only jobs associated with the objects explicitly selected by that
view. Dependency-graph neighbors and other jobs from the same agent do not
expand that selection; unassociated jobs appear only in the global menu.

Counts, search results, and previous/next navigation share that scope. Changing
the selection removes an out-of-scope inspector, and catalog refreshes update
the list. The shared inspector retains summary, output, copy, refresh, and
confirmed termination controls with their existing authority and lifecycle
checks.

Output opens at the newest line of the bounded keeper tail and follows that
edge as running-job refreshes replace the tail. Scrolling away releases the
follow position so earlier output stays put; returning to the bottom resumes
following. Summary continues to open at its top.

The open job, kind or unheard filter and search text are presentation of the
history entry they were made on. Back, Forward and reload reopen them only
within the resident generation that made them, through a fresh catalog read:
a job that read no longer lists stays closed, and the inspector re-reads its
job as it would on a click. No job output, command, result or witness is
written to `history.state`, and a copied `#/spice` address opens Spice itself.

## Design standing

Standing and activity rules live with the
[Design-room contract](DESIGNS.md#design-standing). Design tabs share the
execution tabs' structure, with their count before the stem and their surface
bound to the palette's final accent. The base-flavor contract rebuilds panel,
control, ink, muted, line, ground, focus, and primitive roles from that base
and the page's achromatic anchors; Lane and Design accent slots stay
independent.

Team consoles rank by durable formation instant, oldest first, taking bases up
from the palette's first accent. A team forms at the commit that makes its
identity teamed and keeps that instant until unteamed. Arrangement and browser
order change neither rank; member accents
step from the team's base in durable member order. The task overview wears
that team's base for a team total, the Design rail's plum for Design-only
scope, the operator's neutral for the Drafts scope, and the page's base
otherwise. The Drafts total wears that same neutral.

## Toolbar

The toolbar has two rows: the task strip, then a status band led by the live
ticker and closed by presence pips directly above the composer's team menu, no
wider than that round control, so the ticker runs on over TURBO. The ticker's
age aligns with the composer ages below at every width. The status band is the
console's top edge, ruled in the soft edge tone. Where two consoles touch, their
bands meet flush and square like the tab strips above, and only the left
console draws the half-width hairline divider, in the open tab row's page
colour, so the head reads as continuing down the seam. A corner at the browser
edge stays square.

Pips show how many agents there are and whether each is working, stalled, or
off: one per member in canonical order. Colour and fill are independent
channels. Colour is status: active, queued, bound, or unbound, never identity.
A hollow pip is parked: the resident is holding back that lane's automatic work,
for example a held claim waiting on its own running job, a missing trust grant
or Design decision, a refused start, or a seating hold. The pip turns hollow
when the park is journaled and fills again when it ends, keeping its colour
either way; its label names the reason. For a job park, unrelated resident
reconciliation leaves that standing intact; only the job ending, its admitted
horizon expiring, or ordinary wake input ends the park. One to six stack in at
most two rows bottom-up from the right, a shorter top row nesting half a pitch
in. The ticker keeps age, status, prefix, accent, accessible label, and live
region. A running
command's line names its tool, such as `job:`, then its latest nonblank output
before intent and context, so a narrow ticker cuts context first; hover shows
the bounded line. A new turn names what opened it from the prompt the lane
received: each Spice notice's heading or lead sentence with the facts it lists
(an assignment, a resumed seat's held claim and pending keys, a briefing, room
catch-up, a finished job), each direction key, and each peer note's sender;
the `spice://` guide names nothing, and a turn with no such cause shows the
bare `new turn` label. An empty fleet reserves no pips and invents no status.

## Task strips

The command bar hangs up to three global totals — Drafts, execution, then
Design — from its top edge before the Spice button. Only a scope with open work
renders. The totals fuse into one strip with no interior gap; its Design edge
stands off the button by the button's gap from the window edge. Each team has
one local strip. Totals and strips use the horned-tab construction: team task
and total top rails and global Drafts, Execution, and Design bottom rails are
solid. Every total is badge-only with one fixed footprint.

The Drafts total opens the operator's open Drafts. The execution and Design
totals open all work in their respective scopes; the command bar breaks out no
topic, which its Work deep link reaches. The Design total includes each Design
past plan or review intake and each open task it directly controls; a Design in
intake is execution work. Accountable ownership, never dependency reachability,
chooses the global total, so a Design-produced Plan counts right and the tasks
it creates for execution count left. Local docking never removes a task from
its global total.

Local placement follows
[claim-bound placement](../docs/invariants/identity.md#work-claim-bound-work-has-two-visible-placements):
a team strip tabs each task its lanes actively claim, Design work and Draft
triage included, in that lane's accent. A claimed Draft remains in the global
Drafts inventory while its team tab exposes the active claimant. Local tabs run
right to left in the composers' order, each lane's tabs together over its own
column. A local total selects exactly what its tabs stand for, with no topic
roll-ups. A team total spans the
pip column flush to the console's right edge, centring its badge over the pips
and the team menu. Tabs past a total scroll under edge fades. Local tabs meet
each other and the total without gap, border, or interior rounding; only the
strip's outside ends keep the thick rail and corner.

Each team task tab is one accented shell and one logical group. One and a half
font-relative space widths bracket its age/handle/holders run: the left inset
precedes the elapsed age of the task's first durable claim and the right inset
follows the earliest active claimant at the edge. The age reserves three
character cells; the exceptional `99w+` spelling expands to four and pushes the
handle without consuming that inset. At least one space width separates the
age, neutral handle and holders inside the group. The handle opens task detail.
A later holder, lease renewal, or phase claim never resets that age.
Each lane's current display name wears its accent and opens that lane's primary
Chat composer; a lane not currently admitted to Chat remains a plain label.
Several holders follow latest claim first, so the earliest active claim, whose
accent the tab takes, sits at the tab's right edge. Both targets keep separate
keyboard focus while the tab retains one rail, one background, and no visual
split between them.

An accented tab's identity text is accent-tinted over a 5% accent ground, 10%
on hover, under a top rail of the accent mixed into the line color, stronger on
hover.
Unclaimed handles and totals keep the neutral rail, undimming one step on
hover. Every count badge is one neutral
[open-task total](../docs/loops.md#the-jitter-contract). Gate waits
read the resident's
[one classification](../docs/invariants/identity.md#work-gate-waits-have-one-resident-owned-classification);
only a Blocked row is danger-coloured in detail and graphs. A global or team
total whose partition holds work only the
operator can move now (human-actionable blocked or environment-held work, or
for a Design tab an unclaimed, undeferred Design awaiting decision) carries a
small out-of-flow danger dot on its badge's top inline-end corner and leads its
title and accessible name with that count; the overview's attention rail then
reads Needs attention. Local task tabs carry no mark. Closing any overview
returns focus to its opener.

Topic controls select the exact topic; a relationship control extends the
selection with that task, in the resident's offer order, possibly empty. Work
no first phase has placed forms the Unplaced group ahead of every topic
group; its Topic fact reads Unplaced, with the stored relative tail when the
claimant set one, and opens that group. Once placed, the fact names the topic.

## Team composer controls

The panel holds TURBO, team menu, speech, SEND and Drive/Drain. The menu offers
swap/join, disabled with a reason when no other team exists. Hiding a team
preserves membership and work; a member link restores it and focuses Chat.

TURBO has separate admission. Global TURBO on forces every team and agent
TURBO on and disables those redundant descendant controls; a team TURBO does
the same for its agents. Turning a parent off restores each descendant's own
saved selection and control. Off controls use frosted glass and muted ink; a
lit TURBO uses an amber wash and halo, including one inheriting its parent's
selection. Drive/Drain split one speech-slider-sized pill into two
keyboard targets with independent checked states. A checked Drive uses the
established green wash and halo; Drain keeps the amber treatment. The pill sits
below SEND with clear gaps on both sides and an inset from the panel rule.

Drive admits creation and the draft advance; Drain admits allocation. Its states are off,
Drive, and Drive+Drain: outer states lock the invalid skip; the middle allows
either direction. Checked wash, halo, text and fixed-label opacity settle
together; reduced motion changes state without interpolation. Pending changes
preserve recipients and message controls.

Speech is browser-local to the team input: Quiet / Speak / Narrate, raised by
Up/Right and lowered by Down/Left. Quiet speaks nothing automatically; Speak
voices every live card; Narrate does that and keeps one page-level Media Session
playing across idle gaps so mobile sleep does not retire the narration queue.
Speak and explicit playback hold that same session only while an entry speaks.
An inaudible looping audio element carries it, because platforms surface only
pages playing media; its artist is the speaking agent and its title the current
entry's first line. It clears when the final narrated team leaves Narrate and
nothing speaks.
Steering-key acknowledgments are spoken fully without protocol headers. Other
cards speak first/last meaningful paragraphs; lists are detail (opening list:
first item; closing list: silent). Fenced code, quotes and Markdown marks are
silent. Reduced motion removes slider travel, not states.

SEND layers an even diffuse emitter reaching the glass edge (no ring/hotspot)
under frosted glass with uneven perimeter catches, brighter low-left and fainter
high-right like the sliders. States vary field intensity, pressure and typed rims.
With no member starting/running and nothing drafted, the emitter goes dark,
SEND rests as flush glass, and sliders retain stop accents without catches/halo.

The recipient rim uses actual recipients' accents: continuous for one, arcs for
several, count in the title; never separate targets. Recipients are reachable
members with sendable text, quotes or images, including vacant members to wake.
Running with no drafts means no rim; idle with no agents/drafts disables SEND,
not speech or work switches. Multi-member teams read `FULL SEND` exactly when
every member is reached; broadcast preview uses its actual recipients. Both
labels use `1.5em` relative to the control font without enlarging the disc.

Vacant members receive ordinary durable steering, witnessed without an occupant
at the current generation. After commit the resident launches/resumes the lane,
without browser wake state or another command. Further submissions await a newer
causal lane snapshot before witness reuse; refusal restores the draft.

Pointer, keyboard and touch depress SEND without intercepting activation. Held
submit modifiers add a 2px command ring outside the 3px recipient rim:
focused-agent accent for submit, theme ink for broadcast.

## Speech

Under `spice app` the console speaks through the desktop shell's synthesizer,
because WebKit's `speechSynthesis` offers only built-in voices, not an
operator's downloaded premium voices. The shell injects
`window.spiceShellSpeech` at document start: `getVoices()` lists each voice's
identifier, name, language, quality, and system-default flag; `speak(text,
identifier, rate, handlers)` replaces the current utterance and calls
`onstart`, `onend`, or `onerror` with `{error}`; `cancel()` silences it, after
which its handlers never fire. Requests travel over shell IPC and answers
arrive as evaluated script, so the surface needs no token or content-security
exception. A new document silences what the previous one left speaking. The
speech controller uses the surface, when present, with the same queue,
ownership, voice spread, stop-all lifecycle, and Narrate-only Media Session.
Platform pause and stop controls stop that global queue rather than creating a
second playback owner. Lock-screen metadata names the agent currently speaking
and falls back to `Spice narration` between entries, with stable Spice /
Narration attribution. A browser instead partitions long text at authored
boundaries into utterances of at most 3,000 UTF-16 code units, below
[Android's platform synthesis
limit](https://developer.android.com/reference/android/speech/tts/TextToSpeech#getMaxSpeechInputLength()),
and starts each next utterance only once, from the current chunk's end. Replayed
or late chunk ends cannot repeat a successor. Only the final end settles the
app queue entry; an error or cancellation clears the active utterance and every
remainder. The desktop shell keeps one native utterance because it does not
cross that browser bridge.

## Composer and stream

The composer rides in the Chat plane's scroll content, and its measured height
is the plane's spacer and scroll padding. Message planes never paint a
scrollbar; wheel and trackpad scrolling stay native. Exposure follows [Chat
scroll passively controls composer
exposure](../docs/invariants/lifecycle.md#surface-chat-scroll-passively-controls-composer-exposure).
Programmatic focus moves the caret without scrolling the viewport, fleet,
member strip, or plane; the composer's own focus transition still reveals a
collapsed draft. The fleet opens at its canonical right edge and holds it
through resizes only while the operator leaves it there.

A member strip paints canonical member zero at its right edge, beside the
controls. Tab order instead reads as a person does: one team's member
composers left to right, each heading and its fields in order, then the team's
controls, then the next team. Elsewhere, and inside an open modal dialog, the
browser's order holds.

Each member heading resolves from that lane's newest eligible authored message.
When a history page has none, header demand continues to a newly exposed older
frontier but never reissues the same frontier without fresh viewport demand.
Until one appears, the heading reads `No agent message yet`.

Message and quote signoffs baseline-align the monospace age with the speaker's
name; the age's glyph-height line box keeps the signoff one message line high.

## Input history

Each lane keeps one history of text sent from its composers, quote replies
included, in local storage under the lane's logical identity, so it survives
reloads and occupant changes. A send records each nonblank text it carried,
quote replies first; a repeat of the newest entry adds nothing, and images are
never kept. The document keeps the newest 200 entries per lane within 256 KiB,
trimming the least recently used lanes' oldest entries first; a corrupt or
oversized document is replaced by what fits.

An unmodified ArrowUp that first reaches or rests a collapsed caret on the
first rendered line remains native caret motion; only a repeated Up while that
edge stays armed recalls the previous entry. ArrowDown behaves symmetrically
on the last rendered line, walking forward and finally restoring the unsent
draft. Opposite arrows, selection or caret changes, edits, external body
redraws and focus changes clear the arm. A history replacement re-arms its edge
independently of browser select-event ordering, so continued arrows keep
walking. Home, End, modified arrows and composing input retain their native
behavior. Edits to a recalled entry stay with it while walking; history
changes only on send, which ends every walk and records a sent edit as a new
entry.

Message image runs, authenticated local captures and full-size stepping follow
the shared [image interaction contract](IMAGES.md#message-images).

## Composer activity rail

The trace across the foot of a lane's primary composer follows the
[activity rail contract](TRACE.md). The lane's provider emblem is its menu
button's glyph, and that button's hover title names the provider standing the
emblem names.
