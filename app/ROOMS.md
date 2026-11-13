# Rooms

## Room authority and browser preferences

Every stream subscription, first connect and same-scope reconnect alike,
rebuilds one deterministic projection of room timelines indexed by task stamp,
participant reasons, preparation evidence, decisions, authorizations, and
claim-bound stop requests. The `rooms` subject pages current room authority
at one subscription horizon: every room publication, each decision and
authorization head, each coordination's direction and current steering head.
Every authority fact names its author directly.
Live facts strictly after that horizon continue it, so the operator's
live-start fact stream neither repeats nor misses authority. The projection is
unavailable until its snapshot completes.

The operator's fact stream starts live at the subscription horizon instead of
replaying the journal, so its first `facts` page carries only the newest lane
occupancy facts at or before that horizon (seats, occupant starts and renewals,
and every way an occupancy ends), oldest first, at most one retained page of
rows. The page has no continuation; the stream continues strictly after the
horizon.

Durable task stamps and room publication stamps key cards across reloads and
unavailable source bytes. The current board snapshot resolves an anchor's
displayed handle and terminal standing (the settlement word every surface
shares: done, closed, collapsed, or deleted), an output's control, and whether
a stop's exact claim win is still active. A selected task outside the board's
bounded recent-finished window hydrates through the existing targeted detail
read. A genuinely absent identity is `missing`; absent read authority is
`unavailable`. Persistent preparation parent and controller references survive
dependency changes and decide their own authority independently of discovery.

Snapshot pages share one horizon, rise strictly, and never move backward. A
moved horizon, regressed page, live fact before the snapshot completes beyond
its horizon, live fact at or before it, or stream overflow invalidates the
projection and reconnects for a fresh snapshot. It is never merged
speculatively.

Hiding a team is browser-local: its membership, claims, drafts, lanes, and
running work remain intact, and activating any member's lane link reveals the
team before focusing that lane's Chat composer.

## Design discovery and reading

Designs are tasks. Work's **planning** scope lists them, with settled Designs
under **Settled**, and each opens the existing selected-task view with its
room, decisions, and the decision controls of its completed pass. The Spice menu carries no Design
catalog.

Every Design keeps its own [standing](DESIGNS.md#design-standing), deferral,
pass cycle, requests, decisions, and execution authorizations.
Explicit **Defer** and **Clear** remain the stop and wake controls. Reading a
settled Design or an old permalink uses the same bounded selected-task hydration
as any other task; browsing history never widens the open board.

The task's room composer posts only to that room. A committed reply clears its
draft only if the text and quote shards still match what was submitted; a
refusal retains them. Posting records room evidence and asks no lane for work.
Quoting an operator-authored room card retains its exact
`spice://quote/v1/room/ROOM/PUBLICATION/DIGEST` provenance, shared with team
composers and native delivery. Room posts carry no images.


Room posts and directed lane or principal inputs share one conversation card
family: sender → recipients and text. A lane's presented transcript card retains
every packet's exact source range and authored body, so room projection never
slices that body from rewritten display text. Absent that source, the text its
native delivery carried stands in.

Each directed route appears as its original SYN and keeps its immutable line
identity. Lane and principal endpoints use the same delivery model. A live
route can be pending, queued unheard, or pending against an unavailable or
vacant lane. It can settle as seen, continued, declined, explicitly closed by
FIN, automatically closed at a clean checkpoint, ended unanswered, lapsed,
undeliverable, or cancelled; recovery returns a lapsed route to live pending.
A principal endpoint carries its exact principal identity and short code;
optional profile name and avatar decorate that identity without changing
authority. An ACK with text, NAK reason, or FIN body appears once as that line's
ordinary child input, never as a parallel receipt or `RE` card. Principal FIN
uses explicit Finish because a principal has no lane checkpoint from which to
prove automatic finish.

Quote links in provisional direction text render a complete retained quote,
including attribution, nesting and media, only when the page holds the exact
target: a prose card's quote target or a delivered directive's key and digest.
Otherwise they remain bare links until the source resolves.

Room card words and a directed input's body and quoted direction render as
Markdown through the same renderer task detail uses. A decision's return or
collapse reason and comments remain authored plain text. A message card's
signoff leads its first paragraph; lifecycle state and controls stay outside
the rendered body.

Each recorded decision also joins its Design room's conversation at its
journal position as one card: the deciding operator, its disposition
(returned, concluded, collapsed, accepted, or rejected), a resolution's
Accepted and Rejected output counts as tab-style badges, its reason or
comments, and each output it read with that output's verdict and current
state. The card carries no decision controls; those stay on the Design's
opened Work row while a completed pass awaits decision, as
[Deciding a Design pass](DESIGNS.md#deciding-a-design-pass) describes.

The connected principal's open inputs sit in the Spice sidebar's Principals
section, inside that principal's row beneath its avatar, name, and email, above
Jobs. The heading and open count stay above a viewport-bounded vertical
scroller; ordinary and unbroken card text wrap within the sidebar instead of
widening or clipping the global menu. The heading reads `<initials>'s inbox`
(for example, "CA's inbox") once the connected principal's recorded profile
supplies initials, and "Your inbox" until then; a profile edit retitles the same
surface in place.

Each input names the exact SYN key, sender, route source, current lifecycle
state, and full raw body. A loaded lane transcript provides its authenticated
message card and quote navigation; otherwise the room publication text remains
visible while the route-local `room::conversation-text` read resolves the exact
source. Every live input offers ACK, NAK, and FIN. Bare ACK records a
reaction; ACK with text continues the line, NAK declines it, and FIN explicitly
closes it with an optional final body. Recover returns a lapsed input to
pending. Acting moves keyboard focus to the acting card, and its line effect,
body fate, or refusal is reported beside the input. Sibling routes retain
independent state and delivery cursors.

While the open count is nonzero, the closed Spice button wears one small
neutral dot centered just outside its upper-end rim, clear of the pepper, and
its label names how many messages wait; the dot clears with the last open input
and never shows while room authority loads or reconnects.

The repository-owned OOPS Design uses the ordinary Design room and decision
surface. One typed repository generation authority, not the Design's title or
topic spelling, selects the current open Design. Its report Plans wait behind
it until a pass completes, and the operator decides that pass like any other,
Return included.

## Message cards

Every attributable prose run between nested quote blocks is one speaker-owned
hunk. It opens with the source's Play, Copy, and Quote controls, relative age,
name, and its applicable classification pills: ACK or NAK first, FINAL next,
then BROADCAST. A quoted peer packet keeps its conversation-line standing as
assistive metadata; its protocol transition and root key do not add a visible
badge to the header.
Play speaks only that hunk's rendered text; Copy writes that hunk and every
following hunk in rendered card order, ending in one newline, so the first
control copies the complete card and a later control omits every earlier quote
or reply. Quote targets that hunk's exact authenticated source,
operator-authored included. Direct text after a nested quote starts another
hunk with its source's controls and attribution; the rule applies recursively
to agent prose and resolved operator, agent, and peer quotes.

Attribution is ordinary inline flow, including each sender, arrow, recipient,
classification pill, and the first prose paragraph. A long name or recipient
list therefore starts beside the floated action drawer, wraps between names,
and continues into its pill and prose without reserving an empty drawer-width
line. The drawer remains fixed at the hunk's inline end.

A principal input sourced from a loaded lane transcript embeds that exact
message card inside the line's route, lifecycle, and ACK/NAK/FIN controls. Its
nested quotes retain the team-plane rails, authenticated attribution, Quote and
Play controls, and unavailable-source states. Until that transcript hunk is
loaded—or when a publication has no transcript source—the inbox keeps its plain
loading, unavailable, or room-card text and invents no quote context.


`app-demo` also seeds conversation lines between Iris and Theo, written on their own
transcripts and admitted by the resident's live packet admission: one line
opened, continued, and closed; one declined; one opened by a packet that also
names Juno, whose lane already left the residency, so only Theo hears it; and
one room-only record in the demo task's room. Each quoted line input carries
its standing as assistive `data-message-line` metadata. A sent packet recipient
whose occupant is unresolved exposes `data-quote-recipient-state="unavailable"`
and an assistive unavailable label without restoring a visible protocol badge
or inferring the packet's historical delivery outcome.
The seeded packets carry the same scoped message identity across OMP's start,
streamed block, and settlement records. The loaded card therefore keeps the
room publication's exact streamed locus; a room-only record shows its authored
words without a delivery, while an unloaded source remains unavailable.

Every message Play and Quote action uses the shared message control size and
the same open-edge pair docked at the card's inline end. Play, Quote, and code
Copy use explicit SVG geometry on the same icon grid rather than font glyphs.
Agent-message, Design-publication, Markdown-image, and attachment-image Quote
share one shape, two rounded opening-quote marks. Quote into new Draft follows
Quote with those marks reduced and joined by a plus, and on an image seats
directly beneath its Quote. Copy's overlapping squares stay within the extent
of the Play and Quote marks.
Provider failures lead
their exact error text with the card's relative age and name, without prose
punctuation rewriting. Other non-prose cards keep one attribution-and-action
island in their card corner. A single-image cover places that same unframed
island over its image; it does not introduce another inset panel or action
treatment.

Every rendered code block carries a Copy action seated at its top-right edge,
in the image Quote action's tapered shape, size, colours, and interaction
treatment. The block itself avoids the floated message actions and positions
Copy against the same visible box as its scrolling code, including inside
nested quotes. Copy stays put while wide code scrolls beneath it. It copies
exactly the block's code text, trailing newline included, and the toast reports
`Copied code` or the browser's refusal. Inline code carries no action.

Rendered Mermaid diagrams, including the task-sidebar graph, carry the same
Copy control. It copies their current Mermaid source, not the SVG, and reports
`Copied Mermaid source` or the browser's refusal on the shared toast. Source
stays hidden: no source toggle or pane. The graph's Copy control stays fixed
outside its pan/zoom surface; Markdown diagrams retain their full-size opener.

Every visible inline atom keeps its hunk's `1.25` line pitch: links, task
handles, emphasis, emoji, superscript and subscript, inline math, and quote
attribution never enlarge a line box. The image row therefore begins after
exactly two lines, and its status stays inside the image grid rather than
changing prose leading.

Acknowledgment is an authenticated message disposition, not source text. Every
direct hunk of the answering speaker wears the same ACK or NAK segment, quoted
hunks never inherit it, and pending settlement revises those segments in place.
TASTE marks the quoted directive the resident sent, not the answer. A typed
REVIEW presentation replaces its prose delivery envelope with a finding card;
that card starts with the authenticated Spice source, REVIEW segment, and
reviewed task handle, then closes with typed system guidance that follow-ups are
already boarded and the agent must continue its current task. Its Play action
reads that same task, finding, and guidance in visible order; its Quote action
keeps the authenticated source target while deriving the composer preview from
the typed finding. The discarded envelope reaches neither the actions nor the
rendering, and the lead does not repeat after nested finding quotes. Literal
`[ACK]`, `[NAK]`, `[TASTE]`, or `[REVIEW]` without an authenticated owner stays
ordinary Markdown.

A quoted peer packet keeps its original sender, arrow, and exact destination from
the creating journal fact; a fanout reply quotes only its own delivered
destination. An authored SYN appears as an outgoing quote led by its card's
relative age, then sender → destinations. Its addressed occupants and room/task
target badges share that opening attribution above the body; a room-only post
reads author → task badge. The protocol header remains hidden. An invitation's
message, written to the task's invite URL, has no header; its whole body appears
as one outgoing quote led by author → the invited lane and task badge, whether
that lane was seated and asked or vacant and queued to join.
A lane's name and accent belong to the lane, so an occupant that has since left,
whether its lane is vacant or reseated, still reads under them as a historical
attribution. A lane absent from the roster reads as unavailable, or as the
recipient's address, never as raw identifiers.

A quote whose sole recipient is the lane already named around it leaves that
receiver implied. Around a quote means the prose card that lane spoke, or the
quote card it spoke for a nested quote. The signoff then shows only the age and
sender. The arrow and receiver stay in its accessible name, and its hover title
names both ends, speaker → receiver. Several recipients, another lane, or a
context with no speaker, such as a direction card in the team plane, keep the
arrow and receivers visible.

A room publication authenticates a quoted packet's header- and pill-free body;
other directive prose stays literal. Operator packets and their quotes name
the acknowledging occupant after the arrow. Quote heads keep the card head's
type scale and name color.

An operator direction appears as a provisional card when its `steer::sent` fact
commits, before the receiving transcript records delivery. Its frame keeps its
color: the card's sender rail, every nested quote rail, and every menu island
(including image controls) wear their accents. Sender and nested quote rails
stay solid throughout delivery, with no border animation. Only content
leaves—age, names, arrow, words, badges, and image pixels—read in grayscale,
with no badge and no change to card geometry. The
card's accessible label names the pending transcript delivery. Because the
accepted steering target and retained media identities are already
authoritative, its age links to its stable card and Quote, Play, and image
drawers are present immediately. It attributes its author and receiving lane
exactly as the transcript-backed card for the same lane and steering key will,
so that card enriches it in place: color settles back in while its solid rail
stays unchanged. Words and images keep their nodes, so nothing reloads and a
lightbox open on one of its images stays open.
Cancellation removes the provisional card. Peer and resident notices never
project one.

Each authenticated operator direction a lane's transcript recorded is also a
top-level card in that lane's team plane. Its canonical source row is the
earliest recorded delivery, while its placement and relative age use the
direction's operator-authored instant. The card rests in the operator's neutral
accent. Its body is the direction as one quote reading operator → receiving
members, with that quote's Quote and Play controls and its steering quote
target. One send fanned out to several members of a team is one card naming
each of them, so a resend, a reload, or an older page adds no second
card. The direction's relative age
links to that card as any card's age does, and a quote of the same direction
inside another card links there too; with no direction card in the team plane,
that age stays unlinked. A peer delivery or a post-compaction replay is no
direction card, and the answering ACK or NAK card still quotes the direction
it answers.

Each quote layer ends with one task target after its speaker's last word,
filled with origin-task badges; it repeats on no other hunk, and Play never
reads it. The badges remain in ordinary inline flow, with no floated placement
or reserved minimum height, so the prose uses its full line and wraps the badge
only when needed. An isolated task-handle chip's hover title is its surrounding
activity prose, or `Open task HANDLE` alone. Directly trailing sentence
punctuation stays on the chip's line without preventing a break before it.
A quote's rail shows its speaker: an agent's or peer lane's quote wears that
lane's accent; a direction card's quote wears its first receiving member's;
other operator, Spice, taste, broadcast, review, and unattributed
quotes take the neutral accent; an
unavailable quote keeps its warning rail.

A card's linked relative age is its permalink. Arriving through it outlines
the card briefly, then fades the outline (cleared at once without motion), and
focuses the card without a visible focus ring.

