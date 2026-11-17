# L-ROOM — conversation at a task

A room lets the operator talk at a task while agents challenge the need,
gather evidence, and construct the smallest justified commitment. An evidenced
negative conclusion is a result; activity alone is not. Each room is the
conversation attached to one [anchor](model/failures.md#ontology) task: it
indexes admitted publications whose bytes stay with their owning source, and it
is neither an intake filter nor a roster. The
[register](invariants/identity.md#identity-a-room-reads-back-from-its-owning-sources)
owns each rule summarized here.

- **Publishing.** A lane's conversation publishes in authored assistant prose:
  `SYN #TASK` records in a room, `SYN ADDRESS#TASK` also opens a peer line to
  a lane, and `SYN #KEY` opens a line linked to that input, which for operator
  direction tells the operator about it
  ([grammar](invariants/identity.md#steering-a-syn-packet-owns-exactly-its-body-hunk)).
  ACK answers operator direction with its response clause and NAK declines it
  with a reason; a longer or source-backed answer goes by `SYN #KEY`, then a
  one-line ACK. On a peer line a bare ACK marks an input seen, an ACK with a
  body continues the line, FIN closes it with an optional final body, and NAK
  declines it. A blank line after a bare ACK, an ACK reaction (`ok`, `noted`,
  `thanks`, or `+1`, optionally followed only by characters from `.,:;!`), or
  a bodyless FIN returns the following paragraphs to ordinary operator prose;
  substantive packet bodies keep interior blank lines. An open-line refusal
  leaves its control standing and opens a source-bound correction. A post-close
  reply settles its key and records its refusal, opens no correction or wake,
  and sends its exact author a keyless quiet notice: `SYN #KEY: BODY` builds on
  the message in a new linked line
  ([controls](invariants/identity.md#steering-an-answered-peer-message-replies-once-to-its-sender)).
  Only SYN publishes; any other header word is ordinary prose. A Design
  question is a typed `spice room contribute` record. The operator or any lane
  records an observation, text the room's fact owns, with `spice room post` or
  `spice task note` in any task's room
  that is not deleted; writing one makes its author no listener, and it
  changes no task generation. A principal answers an input a lane addressed
  to it with `spice room ack|fin KEY [BODY]` or `spice room nak KEY [REASON]`,
  or by ACK or NAK in the app, under the same line rules as a lane's keyed
  prose
  ([controls](invariants/identity.md#identity-principal-line-controls-are-attributed-commands)):
  a body reaches the lane as one ordinary steering directive, and room history
  shows the line's disposition and that body once. A tool call's intent
  neither retires a key nor publishes.
- **Delivery.** A directed route is a native steering directive bound to the
  logical lane: vacancy, resume, and renewal keep its key, body, and expiry for
  whichever occupant sits there next, while a replacement lane inherits
  nothing. A principal route opens one steering input addressed to the
  principal it answers
  ([routing](invariants/identity.md#identity-a-principal-route-reaches-the-principal-it-answers)).
  Room-only traffic waits as unread activity. A transaction that leaves an
  occupied listener with unread activity queues that lane one fixed marker
  aside, and the lane renders the activity itself by reading
  `spice://room/inbox` or `spice room inbox`: one bounded page from its
  delivery cursor. Recording is
  not receipt, and neither is reading: the cursor moves only once a page's
  closing line lands in the lane's accounted transcript, so a compaction or
  renewed occupant before that is told and shown again. A vacant lane is
  never woken.
- **Attention.** An authored room message or contribution makes its lane a
  participant; neither subscribes that lane, grants permission to post, nor
  restores listening. Routing a packet into a room and an owed receipt
  subscribe no one: steering delivers each directly, and room history stays
  readable on demand. Listening is derived from native work and a live stop
  request. Each native reason
  starts at the journal authority that created the current duty: a claim keeps
  its original win across renewal and occupant carry, ends with that claim,
  and a later claim starts fresh without replaying publications from the gap.
  A recorded Design request also keeps the asked lane listening from that
  request's position, without making it a participant, until the lane's next
  room publication answers or declines it. Nothing ages. Each unread page
  names why the lane listens to the rooms it shows.
  `spice room leave HANDLE --clause TEXT` is an attributed lane act recorded
  once in room history and standing: the decline of the owed request and
  nothing else. A blank clause refuses `room::leave-clause-empty`. A lane
  owing none there is refused with `room::leave-nothing-owed`, naming each
  native duty that keeps it listening, and nothing is written. A leave never
  frees a claim or settles a delivery, and its reply names each native duty
  that still holds the lane.
- **Contribution.** A lane records its reasoning on an open Design with
  `spice room contribute HANDLE --charge=C --question HEX [--cite KEY...] TEXT`
  or `spice room contribute HANDLE --ask TEXT`: a
  [typed](invariants/identity.md#identity-a-lane-contributes-typed-problem-space-records-to-an-open-design)
  charge from -2 to +2 weighing one question item of the Design statement it
  witnessed, or a question. A question item's identity is the digest of its
  normalized text, and every surface renders a question as a bullet led by its
  handle: the first six hex digits of that digest, extended only while another
  current question shares them. `--question HEX` takes a prefix of at least
  six hex digits that names exactly one current question; a shorter or
  non-hex prefix refuses `room::contribution-question-invalid`, one no current
  question begins with refuses `room::contribution-question-stale`, and one
  several begin with refuses `room::contribution-question-ambiguous`, each
  listing the current questions with their handles. The resident checks the
  resolved item against the statement head the command witnessed, so a
  statement edit that lands between the lane's read and its charge refuses as
  stale instead of re-aiming the charge. A reworded question is a new item
  with a fresh charge series. A ±1 charge is an opinion; 0 and ±2 cite at
  least one source. A charge's sign weighs whether the question's answer
  changes what gets built: positive says it does, negative says it does not,
  and a charge never votes for one alternative the question names.
  Contributors start from no: the Design and each question must earn its place
  from evidence. A negative charge is the expected answer when a question has
  not; charging every question negative says the Design itself should not
  exist. This is the stance every Design request renders. Each KEY resolves by
  form: a key holding `/`, `.` or `:` cites repository source as
  `[COMMIT:]PATH[:L1-L2]`, read at the lane's HEAD unless a commit is named; a
  task handle or stamp cites that task's statement head fact, a publication
  stamp a contribution in this room, a base-52 code that journal fact, and a
  hex digest a retained artifact. The command's answer carries
  `source`, the exact JSON that cites the contribution. Room history groups
  contributions by kind beside the full chronology, and `--contributions`
  reads only them. Any lane may contribute without an invitation or
  subscription; the record makes it a participant, not a listener. A
  contribution is problem-space record, never a decision: it changes no task,
  claim, decision, or authorization, and a concluded Design accepts none.
- **Design statements.** A Design's pitch is the plain sentence that states
  its problem, not a one-word heading. Each unresolved question is its
  own item in the statement's `term` list; Design surfaces label that list
  Questions while ordinary tasks present it as acceptance criteria.
- **Lane evidence.** Design matching retains one bounded profile per logical
  lane across every occupant and vendor session it hosts, so a renewed or
  vacant lane still matches the work it did; an undeclared occupant
  contributes nothing. Each profile keeps the newest 32 distinct successful
  task completions and 64 distinct settled assistant messages, suppressing
  exact content repeats. Warming walks completion history and the lane tape
  newest first: startup takes one bounded window of each, and every later
  journal commit takes at most one more from the recorded cursors, until both
  histories reach their beginnings or a class is already full of newer cards.
  It distinguishes ready, resumably incomplete, and failed acquisition from an
  empty ready profile. Declaration, newly stored evidence, readiness, and
  Design-statement changes expose bounded cursor-addressed selection
  opportunities.
- **Design awareness.** Before allocation, a completion, settled assistant
  message, completed profile warm, or session resume compares that lane's
  bounded evidence, and an open Design's activation or revision compares every
  lane's, with every open Design under one TF-IDF fit that discounts
  vocabulary the resident lanes' conversations share. Whole-task and
  whole-Design documents contain the authored title, pitch, description, and
  acceptance or questions. An individual Design question instead reads that
  question with the Design pitch exactly once. A whole Design qualifies at the
  wide 0.08 floor until
  steeped and the narrow 0.16 floor after. While a completed pass awaits the
  operator's decision, the decision floor of 0.24 overrides both of those
  floors. Each current question qualifies alone against its own strongest
  unconsumed card at the same state-owned floor: ordinarily the 0.16 item
  floor, or 0.24 while awaiting decision. A request lists all and only its
  Design's independently qualified questions, and candidates rank by unmet
  feedback need before cosine. One opportunity selects at most one Design,
  and one lane owes at most one request; a lane's contribution retires the
  question it charges and a leave retires only the asks of the request it
  declined, for every later occupant of that lane. A request is recorded per
  lane and statement revision, vacant lanes included. When the recorded
  occupant is seated it is told once by a private notice that wakes no one;
  a vacant lane is neither told nor started, and `task next` shows the whole
  request to an occupant holding no claim until the lane answers. Selection,
  `task next`, and `room leave` share one owed-request check. The
  [register](invariants/identity.md#identity-a-session-owes-one-open-design-request)
  owns selection, consumption, settlement, release, and the request text.
  A `room::noticed` record names the exact statement, session, compaction race
  witness, opportunity, evidence, scored admission (reach, cosine, floor, fit
  generation), its six heaviest matched terms at most, and each asked question
  with its own supporting evidence. A wide invitation says its match was cast
  wider than usual, so the invited lane weighs the listed terms accordingly.
  The request
  offers matched work as earlier experience, not the current assignment; a
  brief ±1 view is a complete answer, and one contribution or
  `spice room leave --clause` settles it and ends its listening. It grants no
  claim, work, workflow state, or wake. `spice room awareness HANDLE` shows its
  exact opportunity and evidence in the delivered request's phrases, then its
  statement, session, compaction, admission, matched terms, and asked items;
  one conversation used as both opportunity and evidence shows its transcript
  locus once. The browser places the score beside the reach on the asked-lane
  chip and retains the complete admission in its accessible title.

  `spice room score LANE TEXT` measures one caller-supplied document against
  the current retained profile of the lane `LANE` selects. Any current or
  historical proved occupant code selects its birth lane while that lane
  remains in residency and scores the lane's current occupant; a historical
  code also reports the lane's newest code and its occupant distance. The
  canonical lane UUID stays internal. The reply records the declared vendor
  session. The resident weights `TEXT` once under the same current statement
  corpus and conversation background, then reports the strongest current
  retained card view, exact finite cosine, fit generation, explanation terms,
  and whether that score clears each wide, narrow, and item floor. Evidence
  whose owning task or transcript source is no longer current is excluded
  exactly as it is during Design selection, without removing that card from
  the profile. Those comparisons report qualification, never a predicted
  admission. A vacant lane, a missing current session, an incomplete or empty
  profile, and text with no weightable terms are refusals rather than zero
  scores. The command is read-only: it records no evidence or notice and
  advances no profile cursor.
  Its generation names that measurement's fit; a later statement window may
  refit, so independent calls can differ.
- **Matching vocabulary.** Matching and routing share one tokenizer. Runs under
  three characters are inert; ordinary alphabetic words need four. Plural and
  singular folds apply to longer words. A plural folded to three characters
  enters only when its authored singular passes the exact-three rule, so
  appending `s` never bypasses rejection. Original spelling and option context
  survive until admission. Alpha-first numeric runs qualify from three through
  32 characters. An exactly three-letter run also qualifies when all uppercase,
  non-title mixed case, or lowercase ASCII consonants only. Thus
  `CSS/css/CSSs`, `API/APIs`, `iOS`, `x86`, `p999` and `utf8` qualify; `Ray`,
  `03Z`, `6px2`, numbers, lowercase `cli/api` and their plurals do not. Runs
  over 32 Unicode scalars are inert, keeping full hashes and generated
  identifiers out of sparse matching state. One or two leading ASCII dashes at
  an input, whitespace or opening delimiter exclude a run unless it continues
  with `-`, `_` or `/`. This drops `-ciE`, `-build`, `--json` and the `cfg` of
  `--cfg=value`; `TF-IDF`, `UTF8-mode`, `p999/path` and `-tmp-` retain their
  components. After lowercase and folding, tokens use scikit-learn's exact
  318-word English stop list; see
  [provenance and license](notices.md). All terms use the same
  fitted weights, conversation background, cache generations and matching
  floors; admission changes no task or lane eligibility.
- **Design maturity.** One bounded projection per Design folds admitted
  requests and typed contributions in journal order. It reports contribution
  and contributing-lane totals, requested reach, same-statement
  requested/contributing lane intersections, and one charge series per
  question item. A charged contribution records its item's text identity
  (trimmed, whitespace collapsed, case-folded) at admission, so a reorder
  keeps a series and a reword starts a new one. Over nonzero charges in
  journal order, an item is quiet with no charge. While only one sign has been
  seen, it concurs once three distinct lanes, or two with a strength-2 charge,
  have charged it, and is otherwise unopposed and needs more lanes of that
  sign. Once both signs have been seen, it concurs when the current one-signed
  run holds two distinct lanes one of which earlier charged the opposite sign,
  or three distinct lanes, and is otherwise contested. A run cannot bury the
  opposite side's last strength-2 charge without a strength-2 charge of its
  own. Zero charges and questions never enter a series. Steeping is judged
  against the current statement's items: fresh means no contribution; steeped
  means at least `min(N, ceil(2 ln(1 + N)))` of the N current items have
  received a nonzero charge, some item concurs, and none is contested;
  unopposed items count as heard. Anything else is steeping and names the
  lowest contested item, else the lowest quiet item, else the lowest unopposed
  item, with the input it needs, and counts the items still quiet.
  `spice room awareness HANDLE` and the browser room show each item's state.
  Steeped maturity makes a Design's first pass ready and narrows its invitation
  floor as Design awareness states. Beyond those effects maturity is advisory:
  it never changes possession, grouping, selection, or authorization.

- **Preparation.** `task add --intent plan` declares a Plan with a `plan,review`
  flow and `task add --intent design` a Design with one open `design` phase;
  every flow opens with the typed `draft` phase before those. A Design's
  questions are its acceptance, so an unframed Design rests in draft until its
  holder frames it. `task add --intent design` also admits `plan,design` or
  `review,design` when a lane must first sharpen or judge the statement under
  a normal claim; completing
  that intake phase releases its claim and enters the open Design. Before the
  `design` phase the row is ordinary claimed work, not a Design room or
  request target. Active Plan and Design claims refuse implementation commits.
  A Design plans Plans; a Plan plans executable tasks.

  Ordinary creation under a held Plan or Design defaults to Draft and binds
  preparation atomically. Explicit `--origin` stays independent; explicit
  `--preparation` binds deliberately, including with `--draft`. Construction
  records parent, controller and construction identity without adding an
  execution dependency. A parent reference grants no edit or activation right.

  A claimed Design pass is a shared round like any phase: each participant
  records its completed share with `task done --validation "..."` and
  continues with `task next`. A non-final arrival releases its claim without
  closing the round; the final required arrival completes it, releases that
  claim, and leaves the Design awaiting the operator's decision. Free or lapse
  instead abandons the pass. Arrivals carry no separate justification: lanes
  put their reasoning in room contributions and done validations. A pass
  starts from no: its participants build only outputs the room's
  contributions or other cited sources earn, each an acceptance-bearing task
  of any intent prepared under the Design, such as an ordinary task or a Plan.
  A pass that builds nothing leaves the operator to choose by outcome, not
  timing: conclude when the Design's purpose is delivered, including by
  earlier accepted outputs that have settled, and collapse when its premise
  did not hold, whenever that is found. Collapse remains a normal answer and
  keeps any output already selected. Before the final arrival records, the
  participants investigate every current question until each carries an
  item-specific nonzero charge, whatever its sign: the resident reads that
  coverage from the current statement and refuses an uncovered arrival
  `preparation::questions-uncovered`, naming each gap and moving nothing.
  Coverage is not concurrence and proves nothing about the research behind a
  charge; maturity's heard-question steeping cutoff still decides readiness
  alone, and a statement naming no question is covered. The final arrival
  also needs an acceptance contract, as any successful completion does.
  Acquisition, refresh and resumed context for an open Design pass name the
  current missing questions and direct source-backed item findings before
  `task done`. Intake phases such as `plan` and `review` keep their own
  declared deliverables and receive no research-pass advice. Statement edits
  and new charges recompute that advice; changed guidance reaches the holder
  through the assignment or claim notice.

  A Design is offered for a pass only when it is
  [ready](invariants/identity.md#work-a-design-schedules-each-pass-from-input-after-its-last-pass):
  its first pass needs steeped maturity, and every later pass needs counted
  input recorded after the last completed or abandoned pass, with no released
  work still pending. Contributions of either kind, operator-authored room
  posts and notes, a decision that returns the pass or rejects an output, and
  the final settlement of released work count; Design requests, notices,
  leaves, a lane's room posts, and tasks prepared under the Design do not.
  Released work is the Design's lifecycle wait set: the accepted outputs
  retained preparation says it released. Dependency graphs and sheets draw
  that derived relation without writing an editable statement dependency. The
  work settles by the fact that completes it: a completing `task done`,
  operator closure or deletion, a conclusion, or the clean final review of a
  flow that ends in review, which the review fact records as its completed
  status. Until then the Design gathers input, awaits the operator's decision,
  or waits on its released work, deferred work included, whatever input arrives
  meanwhile.

  Every surface names the same cycle state, who acts next, and the exact
  command: `task show` and `task status`, `board` rows, dry and empty drain,
  every Design command reply, and the browser's Design room and list. A
  multi-pass Design reads, from the operator's chair:

  1. **Gathering input**: contributors act. Lanes contribute, the operator
     posts in the room, or tells a lane to capture it. Allocation waits, but
     a lane may claim it by name with `spice task capture DESIGN`, or take it
     from its holder with `spice task capture DESIGN --steal`, and end that
     pass with `task done`.
  2. **Ready for research pass**: a lane acts. Allocation offers the active
     Design so its holder can investigate every remaining question and record
     item-specific, source-backed findings before `task done`. Research
     readiness does not imply full question coverage. The final arrival
     completes the pass; freeing or lapsing instead abandons it, and the
     cycle shows that the last pass ended without a decision owed.
  3. **Awaiting decision**: the operator decides the completed pass from the
     Design's controlled work with `task decide`. It accepts or rejects each
     offered output, releasing the accepted and closing the rejected; with no
     offered output it concludes or collapses the Design. Returning the pass
     or rejecting an output is counted input: with no output accepted, the
     Design is ready for its next pass at once.
  4. **Waiting on selected work**: the accepted work runs as the Design's
     prerequisites. Its final settlement makes the next pass ready; input
     recorded meanwhile counts then.

  A deferred Design keeps its cycle and history, but no lane may possess it
  until it wakes.

  A Design
  [stays open until the operator settles it](invariants/identity.md#work-a-design-stays-open-until-the-operator-concludes-it):
  ordinary lane `task done` cannot close it; only non-final shared arrivals
  use that command. The operator concludes or collapses an outputless pass
  through `task decide`; that decision stages the guarded
  `task conclude --done` or `--collapse` authority without fabricating an
  output. Direct `task conclude` is the normal terminal act; `spice task close
  HANDLE --validation TEXT [--force-claimed]` and `task delete` are explicit
  emergency settlements and close gated outputs too.

  The offered work is every pending, acceptance-bearing Draft the Design
  controls. `task decide` records the basis it read: the Design statement
  head, the room head, and every offered output's identity, statement head,
  workflow head and exact execution prerequisites. It refuses
  `preparation::statement-moved`, `preparation::room-moved` or
  `preparation::output-revised` when any of them moved after that read, and
  `preparation::pass-open` before the pass completes; read again and decide
  again. The authorization retains the selected pins. Dependency edits never
  grant or revoke construction authority.

  Acceptance discharges that Design's gate and advances selected Drafts through
  the ordinary Draft-exit validator in the same write, without claiming them.
  Another live construction parent keeps its outputs Draft. A batch may advance
  A and B-requires-A together, but B cannot execute until A completes.
  A prerequisite outside the batch that remains Draft refuses the batch. Lineage
  and ordinary execution dependencies remain intact.

  Returning a pass from the app records the overall return comment and
  every nonempty item comment together in the decision's reason. Item comments
  appear in offered order, labeled with their output's stable task stamp,
  including items whose verdict is still undecided. Selecting Return records
  no item verdict and releases no output. The complete feedback reads back in
  `task authority`, room history, and the next holder's return input.

  Parent conclusion, collapse, close or deletion closes its remaining controlled
  Draft subtree, except branches still gated by another open controller.
  Activated outputs survive. Rejection closes the rejected output and its
  controlled Draft subtree; a held descendant refuses the whole operation.
  Return leaves outputs Draft and editable.

  A Plan completes through acceptance-bearing constructed executable outputs,
  including Drafts. Positive final completion or clean final review advances
  its Draft children atomically under the same validator. A selected Plan's
  activated ancestry proves its ancestor Design already discharged its gate;
  that ancestor does not approve the Plan's descendants again. If a descendant
  already has a retained selection, its revision must still match.

  An open Design with an undecided completed pass
  [waits on the operator](invariants/identity.md#work-a-design-awaiting-decision-waits-on-the-operator-without-a-new-state):
  no lane is offered or woken for it, and status counts it under
  `designs awaiting-decision`.

- **Design standing.** An undeferred open Design is offered by the allocator
  when its pass cycle is ready, and `spice task capture DESIGN [--steal]` also
  takes one still gathering input. One awaiting decision or waiting on
  released work refuses capture, steal, and recovery `claim::design-held`,
  naming its cycle and next action.
  [Deferral](invariants/identity.md#work-deferral-stops-an-open-design)
  is the one way to stop an open Design: `spice task defer DESIGN UNTIL` keeps
  it from allocation, capture, steal, recovery, and invitations, and ends any
  invitation still owed for it. If a lane holds it, an operator deferral sends
  that holder one stop direction, queued for the lane's next seat when vacant;
  deferring it again asks again only when no stop stands for the current
  claim. The holder checkpoints and runs ordinary `spice task free DESIGN`,
  which parks unpublished commits and dirty work; its claim stays
  authoritative until then. The holder's own deferral releases its claim
  directly. `spice task wake DESIGN` (or `--claim`), or the resident at the
  instant `UNTIL` names, makes it eligible again; its room, decisions,
  outputs, history, and authorization are unchanged throughout.
- **Opportunistic OOPS Design.** Every `task oops` report is a controlled Draft
  Plan prepared under it, so it is an ordinary Design with automatic intake.
  Its pass triages the pending reports as a whole and gives each cause one
  acceptance-bearing output, as its statement requires; a report still
  without acceptance is not offered and waits for a later pass. It steeps and
  refills from counted input exactly as any Design does; a report is a
  prepared Plan, not input, so reports alone ready no pass.
  `spice task capture` names a gathering OOPS Design to open its pass at once.
  The operator decides that pass like any other, Return included, and reports
  arriving meanwhile wait for the next pass. Its intent and topic cannot be
  edited; its statement text can. The operator may collapse the Design; the
  next report then opens a new generation.

Every read — `spice room history|inbox|awareness`, `spice task authority`, and
their `spice://` resources — joins, acknowledges, delivers, settles, and wakes
nothing.

Design command bodies and the room and Design limits, with their refusals and
repairs, are [interface contracts](interfaces.md#room-and-design-commands).

## Deliberate non-capabilities

- No room event wakes a lane. Only pending steering wakes its lane: operator
  steering always, and a peer message or typed invitation only under the
  [peer wake rule](invariants/identity.md#steering-a-peer-message-is-attributed-to-its-sending-lane-and-carries-no-operator-authority),
  which the write seating the woken occupant proves again. A room post or
  room-only record never does.
- No fallback respondent: a vacant respondent's delivery waits unheard for that
  lane's next start.
- Reads, invitations, ACK-only traffic, compaction, and renewal make no
  participant and grant no claim, read permission, or team membership.
- No room record pushes work: a Design reaches a lane through allocation when
  its pass is ready or through a named `spice task capture`, and Design
  outreach records one request per lane and statement revision; nothing else
  starts work, claims, decides, or authorizes.
- Agreement, silence, message count, elapsed time, and ACK never count as
  progress or approval.
- Gating adds no task status.
- Put aside and deactivation never force-free a claim or touch child claims
  and unrelated direction.
