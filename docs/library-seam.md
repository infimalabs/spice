# Library Seam

Mounted commands and tracked pre-commit extensions may import a deliberately
narrow Python seam from `spice` instead of vendoring harness scaffolding. This
surface is source-stable for target repositories: public names in the modules
listed here are not removed or renamed silently, and incompatible changes
require an explicit contract update. Underscored names remain private.

## Public Modules

- `spice.errors`: `SpiceError` for user-facing command failures.
- `spice.policy`: constitution constants and `flex_limit`.
- `spice.flexstate`: flex-limit sticky-state persistence and rename helpers.
- `spice.locking`: cross-platform advisory file locks.
- `spice.paths`: repo-root, state-dir, atomic write, and tool-resolution helpers.
- `spice.repocfg`: tracked `[tool.spice]` table readers.
- `spice.studies.walk`: tracked/staged path walkers, repo policy exclusions,
  staged renames, and git blob reads.

## Process Groups

`spice.procs` provides cross-platform POSIX and Windows helpers for spawning,
checking, and terminating process groups:

- `popen_new_process_group_kwargs()` returns platform-appropriate
  `subprocess.Popen` keyword arguments for a new process group.
- `terminate_process_group(process, *, signum=None, timeout_seconds=2.0)`
  gracefully terminates a process group with fallback to force-kill after the
  timeout.
- `terminate_process_group_id(pgid, *, signum=None)` terminates a process group
  by ID.
- `process_group_is_running(pgid)` checks whether a process group is alive.
- `process_id_is_running(pid)` checks whether a process ID is alive.

## Study Helpers

The study modules `spice.studies.fileloc`, `spice.studies.complexity`,
`spice.studies.magicnums`, and `spice.studies.envpolicy` expose finding
dataclasses plus `scan_*`, `detect_*`, and `render_*_board` helpers for
project-specific studies.

The flex and sticky scans
(`scan_staged_loc_violations`, `scan_staged_complexity_violations`) are pure
queries by default. Only a committing gate passes `persist=True` to advance the
sticky floor, and it must pair that with the matching
`clear_file_loc_sticky_state` or `clear_complexity_sticky_state` on gate
success. Running one half without the other causes permanent ratcheting or no
release at all.

Everything else is an internal implementation detail unless this document names
it. A repo tool that needs an unlisted helper should vendor that helper or first
add it to this seam with tests and a stability note.
