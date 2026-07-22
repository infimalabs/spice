"""User-facing failure: printed as `spice: <message>`, exit code 2.

Refusal messages lead with the repair. A reader who has just been stopped
wants the way out first, and the message is often the whole interface they
get -- an agent mid-claim has no operator to ask and no second screen to
read. So a refusal that can name a way out states the executable step, then
`; `, then the diagnostic that explains it:

    run `spice task claim ABC-123 --steal` to repair ownership; task claim
    blocked: ABC-123 is ACTIVE but has no claim_by

Two kinds of refusal are deliberately exempt, because inverting them would
put the reader further from the fix rather than closer:

- A refusal with nothing to run stays a bare diagnostic. Inventing a repair
  to satisfy the shape is worse than admitting there is none.
- A refusal about the invocation itself -- a missing, invalid, or
  conflicting argument -- stays a single contract statement (`task depends
  requires --after and/or --not-after`, `invalid priority X (use
  critical/high/medium/low/none or C/H/M/L)`). Naming the bad argument is
  already naming the fix, and splitting one clause in two would only make
  it longer.

Gate reports are a third shape and follow their own layout: a rendered
finding board, then the summary that scores it. The board is the evidence,
not a diagnostic to be led past.
"""

from __future__ import annotations


class SpiceError(RuntimeError):
    pass
