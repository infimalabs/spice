# Composer activity rail

A lane's trace runs unframed and flush across the foot of its primary
composer, a 32-pixel rail behind the draft covering the last ten minutes of
that lane's activity at the rail's own measured size. The rail takes no
pointer input, drains to gray while the lane's seat is empty, and fades out
once the draft holds text. Quote shards carry no rail.

## Curves

Each kind of work, reasoning, tool, and prose, whose level reaches 2% of the
graph somewhere in the window draws one smoothed curve over a fill fading
toward the floor, ending in a dot at now; in the dark theme the curves glow.
The curves keep a four-pixel headroom under the rail's top, so the graph
itself is 28 pixels tall. A curve's line strokes only where it leaves the
floor, so an idle stretch shows nothing, and a lane with neither visible
curves nor markers in the window draws no trace.

Reasoning and prose rise with how often and how much the lane thinks and
writes. The tool curve rises with the tool output returned into the model's
context each minute, however many calls returned it: a call that is still
running or returns nothing adds no height, a run of small edits stays low, and
a run of large reads stands high. Its height is the square root of that rate's
share of 64 KiB a minute, so quadrupling what the lane reads doubles the curve.

## Gaps and markers

The rail has gaps where the lane was off, from an occupant's end or a vacant
seat to the next occupant's start. An open gap cuts the curves at that
recorded end immediately; only a completed gap shorter than one displayed
sample spacing is elided. A reload keeps those gaps from the stream's recent
occupancy.

Turn, direction, provider failure, compaction, fresh session, and sign-out
draw as markers over the curves, the rules spanning the whole rail and the
direction dots sitting in the headroom, so every marker stands clear above the
highest peak. The operator's direction dot is neutral ink; a SYN another
rostered lane sent this lane draws the same dot at its send instant in the
sending lane's accent, while the sender's own rail, a send that names no lane,
and a sender outside the roster mark nothing.

## Motion

The rail redraws on new activity and, while it draws anything, every four
seconds. At each four-second boundary the drawn curves, gaps, and markers slide
left by one step of the window in a quarter-second transition, then rest until
the next boundary, while the now-dots hold the right edge; reduced motion moves
to the new frame without sliding.

## Drawing

The app's `style-src 'self'` policy refuses inline style attributes, so the
rail is SVG geometry in the pane's measured CSS pixels, every colour and glow
comes from the stylesheet, and the slide's span and duration reach it as
custom properties written through CSSOM. A browser check of it must keep that
policy enforced: a context with `bypassCSP` would hide a drawing that
draws nothing in production.

Yew's `html!` lowercases literal element names, and SVG recognises
camel-case elements such as `clipPath` only by their exact case, so the rail
writes those as dynamic tags (`<@{"clipPath"}>`).
