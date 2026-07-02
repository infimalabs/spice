# Serve UI Lit Island Prototype

Status: closed and decommissioned, 2026-06-22.

## Decision

Keep the metrics pane on the component-shaped vanilla renderer. The Lit pilot
proved a no-build Web Component island could be mounted, but it duplicated the
metrics renderer and did not solve a problem the vanilla renderer could not
handle.

The former opt-in path was removed. The serve UI no longer supports a Lit
metrics toggle or ships a separate metrics Lit module.

## Rationale

Lit is a real lightweight Web Components library. It is a reasonable candidate
when the component boundary buys enough clarity to offset another rendering
model. The metrics pane did not meet that bar:

- The production vanilla renderer already owns the live browser coverage.
- The metrics chart is plain SVG plus three controls, not a complex component
  hierarchy.
- The prototype imported Lit from a CDN, which was acceptable for a prototype but
  did not make the graph itself more capable.
- Keeping both renderers forced every metrics fix through two code paths.

The chosen path is one renderer: vanilla DOM helpers in `app.panes.js`, backed by
browser smoke coverage for graph ordering, full-width SVG behavior, stable
controls, and summary cells.

## Future Graph Libraries

A dedicated graphing library remains an option if the chart requirements grow
beyond the current SVG helper. Any candidate should replace the existing chart
path directly rather than adding a second optional renderer.
