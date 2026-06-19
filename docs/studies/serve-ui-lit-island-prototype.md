# Serve UI Lit Island Prototype

Status: prototype, disabled by default, 2026-06-19.

## Candidate

The lane metrics pane is the pilot surface. It is non-core, read-only, already
has a compact data model, and does not participate in pointer-heavy lane drag,
composer submission, team routing, or filter mutation. That makes it a fair
place to measure component ergonomics without putting operator control flows at
risk.

The prototype is opt-in through `?litMetrics=1` or
`localStorage["spice.serve.litMetrics"] = "1"`. Without the opt-in, the metrics
pane continues to render through the vanilla DOM path.

## Code Size

The previous metrics renderer was about 44 lines inside `app.panes.js`, including
cell creation, sparkline rendering, and duration formatting. The prototype adds:

- 67 lines in `app.metrics-lit.js` for the Lit custom element and renderer
  adapter.
- 98 inserted and 20 removed lines in `app.panes.js` to split a shared render
  model, keep the normal vanilla renderer as the non-Lit baseline, and gate the
  Lit loader.
- 1 CSS line for `.lane-metrics-lit-island { display: contents; }` so the
  custom element does not disturb the existing grid.
- 257 lines of focused test coverage, mostly a small fake DOM fixture that keeps
  the opt-in loader behavior executable without a browser.

The component itself is readable, but the adapter and opt-in scaffolding are
larger than the original pane. That overhead is acceptable for a prototype and
too heavy to justify default adoption for a panel this simple.

## Test Clarity

The shared `laneMetricsRenderModel` improves testability for both renderers. The
Node fixture stubs `window.__spiceLitMetricsModuleLoader`, verifies the default
vanilla path never loads Lit, verifies the opt-in path renders vanilla while the
module loads, and verifies the Lit renderer receives the same model after the
loader resolves.

That is a cleaner seam than asserting raw DOM strings, but it still leaves the
actual Lit template behavior to browser validation because LitElement,
custom-elements upgrades, and module import timing are browser behavior. A real
Lit migration would need a reusable browser harness around custom element
upgrade and render completion.

## CSS And Event Friction

Shadow DOM is the main CSS friction point. The existing metrics pane uses global
CSS variables and `.lane-metric-*` selectors. The prototype overrides
`createRenderRoot()` to render into light DOM and adds `display: contents` to the
custom element so existing grid placement still applies.

That avoids duplicated styles, but it also means the island is using Lit for
templating and lifecycle more than encapsulation. The metrics pane has no local
events, so this pilot does not answer how Lit interacts with the existing
document-level event delegation, pointer capture, focus dismissal, or optimistic
team/filter mutation flows.

## Static Serving Compatibility

Lit's own getting-started documentation describes a pre-built, dependency-free
bundle import for no-build workflows:
`https://cdn.jsdelivr.net/gh/lit/dist@3/core/lit-core.min.js`.

The prototype uses that bundle from `app.metrics-lit.js`, loaded only after the
operator opts in. Static serving still works with no Python route changes and no
build step. While the opted-in module is loading, the pane paints once through
the vanilla renderer; if the Lit module cannot load or does not export the
expected renderer, the opt-in path reports a browser error instead of quietly
preserving the vanilla path.

The compatibility gap is offline/locality. The default serve UI should not
depend on a remote CDN. Before enabling Lit by default, spice would need either a
vendored bundle served from `/static/`, an npm/client asset policy, or an import
map plus packaging story that survives source checkouts and installed wheels.

## Decision

Keep the serve UI on component-shaped vanilla modules by default. The Lit island
is useful as an experiment and confirms that a no-build island can be mounted
without disturbing the page, but broader adoption is not justified yet.

Revisit Lit only after one of these becomes true:

- A local Lit bundle or package-serving policy is in place.
- A second pilot covers an eventful panel such as filters or a contained team
  menu section.
- The vanilla seams stop giving enough test clarity for state derivation and DOM
  adapters.
