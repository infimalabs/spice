# Serve UI Frontend Framework Path

Status: recommendation, 2026-06-19.

## Recommendation

Do not rewrite the serve UI into a full framework now. First keep the static
no-build app and extract testable seams: state derivation, routing decisions,
drag/drop geometry, optimistic updates, and render adapters. That is the
shortest path to better tests without moving far from the current system.

If those seams are still not enough, pilot Lit as an island component layer for
one contained surface. Lit is the best first framework candidate because it is
based on Web Components, uses reactive properties and declarative templates,
and does not require compilation. That matches the current Python-served static
asset model better than a whole-app React, Preact, or Svelte migration.

## Current Constraints

- The UI is a localhost operator tool served as static files by Python.
- The static browser surface is about 10k lines across focused JS files and CSS.
- The current web gate is TypeScript `checkJs`; the repo-local Node dependency
  is Playwright for browser validation.
- Hard parts are not simple templating. They are pointer-heavy drag/drop,
  live-bus reconciliation, optimistic team/task routing, and preserving operator
  state across refreshes.
- A build step would be new operational surface for every agent and release.

## Options

### Vanilla JS With Seams

This should be the next move.

Extract pure functions and small controller objects from the current DOM-heavy
files, then test them directly. Keep DOM adapters thin and browser-test the real
pointer workflows with Playwright. This improves correctness while preserving
the current deployment shape.

Good candidates:

- lane/team route derivation
- task filter inventory selection
- drag/drop hit testing and insertion zones
- optimistic team membership updates
- pending inbox/ACK count reconciliation
- lane view state and persistence

Tradeoff: this does not give framework-level component ergonomics. It does
remove the main testability problem first, which is mixed state, geometry, and
DOM mutation.

### Lit Islands

Lit is the best framework candidate if we decide a component layer is needed.
Its official docs describe reactive properties and declarative templates, and
the docs explicitly position Lit templates as requiring no compilation. That is
a good fit for static serving and incremental migration.

Use Lit only as islands at first, such as a filter editor, team menu section, or
metrics pane. Avoid wrapping the live lane shell until the state seams are
extracted.

Tradeoffs:

- Shadow DOM can fight the existing global CSS and event delegation; use light
  DOM patterns or opt out where needed.
- Custom-element lifecycle is another model to learn.
- It helps component boundaries, not core drag/drop geometry by itself.

### Svelte

Svelte has excellent component ergonomics and compiles components into browser
JavaScript, which can produce small runtime output. It is attractive if we were
starting a product UI from scratch.

For this repo, Svelte is not the first move because it introduces a compiler and
bundle workflow as a prerequisite. That is a larger operational change than the
current problem requires.

### React Or Preact

React and Preact are good component systems. Preact plus Signals is especially
interesting for fine-grained state updates. They are still not the first move
here because JSX/component bundling, virtual DOM ownership, and imperative
drag/drop integration would force a larger app-shell migration before we have
clean state seams.

## Migration Plan

1. Add a reusable Playwright harness for `spice serve` smoke tests.
2. Extract pure route, filter, drag geometry, and optimistic-update helpers from
   the largest static files.
3. Add direct unit tests for those helpers and keep browser tests for the real
   pointer workflows.
4. Convert one low-risk panel to a component-shaped vanilla module.
5. If component-shaped vanilla still feels too heavy, prototype the same panel
   as a Lit island and compare code size, test clarity, CSS friction, and
   browser behavior.
6. Decide after the island pilot. Do not migrate the lane shell first.

## Follow-Ups

- Extract serve UI state and drag geometry helpers from DOM adapters.
- Add a reusable Playwright harness for serve UI interaction checks.
- Optional after those land: prototype one Lit island in a non-core panel.

## Sources

- Svelte describes itself as a compiler-based UI framework that outputs browser
  JavaScript and CSS: https://svelte.dev/ and https://svelte.dev/docs/kit
- Lit documents reactive properties and declarative templates, with no
  compilation required for templates: https://lit.dev/ and
  https://lit.dev/docs/components/properties/
- React positions itself around componentized UI: https://react.dev/
- Preact Signals document reactive primitives for state-driven UI updates:
  https://preactjs.com/guide/v10/signals/
