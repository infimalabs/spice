# Release

Releases are cut from a clean synchronized worktree with this repository's
mounted `spice release` command. Lane branches are allowed; the release command
pushes the prepared release commit to `origin/main`.

```sh
spice release range           # preview latest-release-tag..HEAD before prepare
spice release prepare minor   # bump, validate, commit, stop before publish
spice release notes > /tmp/spice-release-notes.md
spice release publish --notes-file /tmp/spice-release-notes.md
spice release minor           # one-pass bump, validate, commit, publish
```

Release validation runs every scratch-safe served-UI Playwright scenario from
`tests/browser/release_smoke_manifest.js` using the repository's pinned
`playwright` dependency. Run `npm ci` in the repository before releasing. The
release gate materializes the shared browser configuration through Spice's
canonical worktree-state resolver and passes its absolute path to the Node
harness, so a clean checkout does not depend on a prior agent launch or a
repo-visible config.
The manifest classifies every `tests/browser/*_smoke.js` file: scratch-server and
page-local fixtures are mandatory, while scenarios that create or depend on
live external state are listed explicitly with a reason and must be run in a
suitable live lane.

The release command removes ignored `build/` and `dist/` trees before the
constitution gate and again before assembling artifacts. This prevents files
deleted from the source tree from surviving in setuptools' reusable build
directory and entering a release wheel.
The artifact gate then imports `spice.config.layers` with Python's isolated
mode before exercising the installed CLI, so checkout paths and `PYTHONPATH`
cannot mask a broken wheel.

Before `prepare`, the bare `spice release range` command resolves the highest
version tag merged into the current `HEAD` and previews `latest-tag..HEAD`
without requiring a future version literal.

For curated GitHub release notes, generate the draft after `prepare` and edit
from that file instead of relying on session memory. The draft is built from
first-parent commits in the exact previous-release-tag-to-release-commit range,
grouped by landed task project metadata under a `## Changes by project` section,
and records that range in the package notes. That grouped export is a **draft,
not the final release body**: it opens with a curation banner and an empty
`## Highlights` placeholder, followed by the raw project-grouped inventory in a
ready-made collapsed `<details>` section. Fold the changes into a short set of
human-readable highlights, delete the banner and placeholder, and keep the
generated details section intact. Task-level commit SHAs are deliberately left
bare so the GitHub release page renders them as repository commit links. A
release that still shows the draft banner was shipped uncurated.

Bare `spice release notes` is state-aware: before `prepare` it labels the draft
`unreleased`; after the bump commit it recognizes the untagged current version
and writes versioned package and release-tag markers.

When release history is unusual, pass `--release-commit <rev>` to choose the
commit used for `spice release range`, `spice release notes`, or
`spice release github`. Use it for tag repair, delayed publication, or a
prepared version whose correct release target is not the default resolver.
`spice release publish --release-commit` is stricter: the commit must resolve
to `HEAD`, because publish builds and uploads artifacts from the current
worktree before creating the GitHub release.

Use a minor release when users can do something new or observe changed
behavior: new commands or flags, new configuration, new `spice serve` or task
workflow behavior, extension entry-point surface changes, changed output or
artifacts, or any compatibility break while the project only has patch/minor
release lanes. If a release contains both patch-level fixes and minor-level
surface changes, choose minor.

Use a patch release only when the shipped contract is unchanged: bug fixes,
documentation clarifications, packaging fixes, or internal test/build/tooling
changes that do not give operators a new capability and do not alter CLI,
configuration, UI, task/session semantics, extension entry points, or command
coupling behavior.
