# Release

Releases are cut from a clean synchronized worktree with this repository's
mounted `spice release` command. Lane branches are allowed; the release command
pushes the prepared release commit to `origin/main`.

```sh
spice release range           # preview the prior-tag..release-commit landed tasks
spice release prepare minor   # bump, validate, commit, stop before publish
spice release notes > /tmp/spice-release-notes.md
spice release publish --notes-file /tmp/spice-release-notes.md
spice release minor           # one-pass bump, validate, commit, publish
```

Release validation runs every scratch-safe served-UI Playwright scenario from
`tests/browser/release_smoke_manifest.js` using the repository's pinned
`playwright` dependency. Run `npm ci` in the repository before releasing. The
manifest classifies every `tests/browser/*_smoke.js` file: scratch-server and
page-local fixtures are mandatory, while scenarios that create or depend on
live external state are listed explicitly with a reason and must be run in a
suitable live lane.

For curated GitHub release notes, generate the draft after `prepare` and edit
from that file instead of relying on session memory. The draft is built from
first-parent commits in the exact previous-release-tag-to-release-commit range,
grouped by landed task project metadata under a `## Changes by project` section,
and records that range in the package notes. That grouped export is a **draft,
not the final release body**: it opens with a curation banner and an empty
`## Highlights` placeholder. Fold the changes into a short set of human-readable
highlights, delete the banner and placeholder, and keep the raw list only as a
collapsed `<details>` appendix if useful. Do not publish the raw export — a
release that still shows the draft banner was shipped uncurated.

When release history is unusual, pass `--release-commit <rev>` to choose the
commit used for `spice release notes` or `spice release github`. Use it for tag
repair, delayed publication, or a prepared version whose correct release target
is not the default resolver. `spice release publish --release-commit` is
stricter: the commit must resolve to `HEAD`, because publish builds and uploads
artifacts from the current worktree before creating the GitHub release.

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
