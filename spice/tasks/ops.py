"""Task mutations: add, claim, done, review, oops, notes, dependencies.

Every operation is a thin, guard-railed compile from agent intent to native
Taskwarrior.
"""

from __future__ import annotations

import json
import subprocess
from collections import Counter
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Sequence

from spice.config import configured_rtk_executable
from spice.errors import SpiceError
from spice.hooks import install as hook_install
from spice.hooks import precommit
from spice.paths import repo_root_from_cwd
from spice.sessions import learnings as session_learnings
from spice.sessions import records as session_records
from spice.sessions import resolve as session_resolve
from spice.tasks import alloc, config, gitsync, identity, reviewfeedback, tw
from spice.tasks.claimstate import (
    CLAIM_CLEAR,
    _claimed_task_capture_recovery_message,
    _live_claim,
    _live_claim_text,
    _record_task_lifecycle_event,
    _require_manual_claim_allowed,
    _require_owner,
    _require_pending,
    _require_plan_phase_done_has_no_local_commits,
    _require_single_active_slot,
    _task_continuation_contract,
    annotate,
    denotate,
    do_claim,
    phase_index,
    phases_of,
    require_no_active_plan_phase_implementation,
    resolve_claim_target,
)
from spice.tasks.projectsubs import (
    _gc_empty_project_task_filters,
    _subscribe_claim_project,
    _subscribe_created_project,
    _subscribe_woken_project,
)

# ---- claim --------------------------------------------------------------


def claim(handle: str, *, steal: bool = False) -> str:
    row = identity.resolve(handle)
    _require_pending(row, "claim")
    actor = tw.current_actor()
    _require_manual_claim_allowed(row, actor)
    _require_single_active_slot(actor, action="task claim", target=row)
    owner = str(row.get("claim_by") or "")
    if owner and owner != actor and not steal:
        raise SpiceError(f"task already claimed by {owner}; use --steal to take it")
    if row.get("start") and not owner and not steal:
        raise SpiceError(
            "task is ACTIVE but has no claim_by; use --steal to repair ownership"
        )
    uuid = identity.uuid_of(row)
    guarded = not steal and owner != actor
    # A fresh claim (not a repair of our own already-active row) brings the
    # tree to the current baseline before the claim records its commit.
    is_repair = owner == actor and bool(row.get("start"))
    notes = [] if is_repair else gitsync.prepare_for_claim().notes
    if not do_claim(uuid, actor, guard_unclaimed=guarded):
        raise SpiceError(
            "claim lost a race: task became active before this claim landed; "
            "run task next again"
        )
    if owner and owner != actor:
        annotate(uuid, f"claim stolen: {owner} -> {actor}")
    _subscribe_claim_project(row, actor)
    handle_text = identity.render_handle(identity.resolve(handle))
    claim_lines = [handle_text, claim_drive_line(handle_text)]
    if notes:
        return "\n".join([*(f"task: {n}" for n in notes), *claim_lines])
    return "\n".join(claim_lines)


def claim_drive_line(handle: str) -> str:
    return (
        f"drive: continue {handle}; drive the current phase to completion "
        "with normal validation"
    )


# Nudge only with enough local signal and genuinely poor compaction, so it stays
# occasional and silent once the agent feeds rtk well (or rtk is absent).
RTK_NUDGE_MIN_COMMANDS = 6
RTK_NUDGE_SAVINGS_FLOOR_PCT = 12.0


def rtk_usage_nudge() -> str | None:
    """One-line rtk-feeding reminder, emitted only when rtk savings are poor.

    Reads rtk's current-project gain summary (`rtk gain --project -f json`) so
    the reminder is self-correcting and silent when this tree is already feeding
    rtk well, when there is too little signal, or when rtk is not installed.
    """
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        return None
    try:
        rtk_executable = configured_rtk_executable(repo_root)
        completed = subprocess.run(
            [rtk_executable, "gain", "--project", "-f", "json"],
            cwd=repo_root,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            check=False,
            timeout=5,
        )
    except (OSError, SpiceError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0:
        return None
    try:
        summary = json.loads(completed.stdout)["summary"]
        commands = int(summary["total_commands"])
        avg_savings = float(summary["avg_savings_pct"])
    except (ValueError, KeyError, TypeError):
        return None
    if commands < RTK_NUDGE_MIN_COMMANDS or avg_savings >= RTK_NUDGE_SAVINGS_FLOOR_PCT:
        return None
    return (
        "rtk: command output is barely compacting -- run read-heavy commands "
        "discretely (no heredocs/subshells) and let rtk shrink full output "
        "instead of pre-tersing with --oneline/-s/|head/|tail"
    )


def unclaim(handle: str | None = None) -> str:
    row = resolve_claim_target(handle, action="unclaim")
    uuid = identity.uuid_of(row)
    # Atomic: clear the start date (deactivate) and the claim metadata together.
    tw.run([uuid, "modify", "start:", *CLAIM_CLEAR])
    return identity.render_handle(row)


def wake(handles: Sequence[str], *, into: str | None = None) -> str:
    """Clear delayed task waits so the allocator can see them as current.

    Bare wake un-defers in place and refuses hidden oops triage rows. With
    ``into``, promotion becomes explicit: the same wait-clear plus a project
    move into the named public project, which is how a deferred hidden-board
    task (.oops) enters the active public queue. Identity rides on the
    project string alone, so the project move is the whole promotion.
    """
    if not handles:
        raise SpiceError("task wake requires at least one handle")
    target = None
    if into is not None:
        target = config.validate_manual_creation_project(into)
    rows = [identity.resolve(handle) for handle in handles]
    for row in rows:
        _require_pending(row, "wake")
        rendered = identity.render_handle(row)
        if target is None and alloc.is_oops(row):
            raise SpiceError(
                f"cannot wake deferred oops triage task: {rendered}; "
                "promote it with wake --into <public-project>"
            )
        if row.get("start") or str(row.get("claim_by") or ""):
            raise SpiceError(f"cannot wake active or claimed task: {rendered}")

    mods = ["wait:"]
    if target is not None:
        mods.append(f"project:{target}")
    tw.run([*(identity.uuid_of(row) for row in rows), "modify", *mods])
    fresh = [identity.render_handle(row) for row in rows]
    actor = tw.current_actor()
    if target is None:
        projects = tuple(
            dict.fromkeys(str(row.get("project") or "").strip() for row in rows)
        )
        woke_lines = [f"woke {handle}: wait:" for handle in fresh]
    else:
        projects = (target,)
        # The handle prefix tracks the project, so promotion renames the
        # task; re-export to report the identity the operator uses next.
        promoted = [
            identity.render_handle(tw.export([identity.uuid_of(row)])[0])
            for row in rows
        ]
        woke_lines = [
            f"promoted {old} -> {new}: wait: project:{target}"
            for old, new in zip(fresh, promoted, strict=True)
        ]
    route_feedback = [
        _subscribe_woken_project(project, actor) for project in projects if project
    ]
    lines = [
        *woke_lines,
        *route_feedback,
        "next: spice task next",
    ]
    return "\n".join(lines)


def edit(
    handle: str,
    *,
    priority: str | None = None,
    project: str | None = None,
    acceptance: list[str] | None = None,
) -> str:
    """Change an existing task's priority, project, and/or acceptance in place.

    Avoids the delete-and-recreate detour for a simple priority bump, a
    project move, or a plan task gaining its bookend acceptance: resolve the
    task and apply whichever fields were supplied in one modify. At least one
    field is required. Acceptance replaces the prior value wholesale, joined
    the same way creation writes it, and the new text passes the same
    suspect-wording scan creation runs — a match sets the review marker so a
    plan task still self-corrects before advancing.
    """
    from spice.tasks import create
    from spice.tasks.wording import detect_task_creation_wording

    if priority is None and project is None and acceptance is None:
        raise SpiceError("task edit needs --priority, --project, and/or --acceptance")
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    mods: list[str] = []
    if priority is not None:
        mods.append(f"priority:{config.map_priority(priority)}")
    resolved_project: str | None = None
    if project is not None:
        resolved_project = config.validate_manual_creation_project(project)
        mods.append(f"project:{resolved_project}")
    wording_matches: tuple = ()
    if acceptance is not None:
        _require_pending(row, "edit acceptance for")
        items = [item.strip() for item in acceptance if item.strip()]
        if not items:
            raise SpiceError("task edit --acceptance needs at least one entry")
        mods.append(f"acceptance:{' | '.join(items)}")
        wording_matches = detect_task_creation_wording(title="", acceptance=items)
        if wording_matches:
            mods.append(
                f"{config.TASK_WORDING_REVIEW_UDA}:{create.TASK_WORDING_REVIEW_MARKER}"
            )
    tw.run([uuid, "modify", *mods])
    if wording_matches:
        annotate(uuid, create._suspect_wording_annotation(wording_matches))
    lines = [f"edited {identity.render_handle(row)}: {' '.join(mods)}"]
    if resolved_project is not None:
        lines.append(
            _subscribe_created_project(
                resolved_project, tw.canonical_actor(tw.current_actor())
            )
        )
    return "\n".join(lines)


# ---- capture ------------------------------------------------------------


def _capture_default_title() -> str:
    """A task title derived from the most recent loose commit subject."""
    from spice.tasks import create

    subject = tw._git("log", "-1", "--format=%s").strip()
    if not subject:
        return "Capture loose commit"
    return subject[: create.TASK_TITLE_LIMIT].strip()


def capture(
    handle: str | None = None,
    *,
    title: str | None = None,
    project: str | None = None,
    description: str | None = None,
    priority: str = config.DEFAULT_PRIORITY,
    complete: bool = False,
    validation: list[str] | None = None,
    origin: str | None = None,
) -> str:
    """Fold loose commit(s) into a task and capture them through the normal flow.

    A loose commit is one made while no task was claimed — before any claim,
    or after the previous `task done`. `task next` refuses to start new work
    while a loose commit sits ahead of the baseline. `capture` claims a task —
    newly minted, or the given handle — over those commits *without* the
    baseline fast-forward a normal claim performs, so the work is preserved
    rather than rejected and the agent finishes it through the usual `task
    done`/`review` flow.
    """
    validation = list(validation or [])
    if validation and not complete:
        raise SpiceError("task capture --validation requires --done")
    if complete and not validation:
        raise SpiceError("task capture --done requires --validation")
    tw.require_clean_worktree("task capture")
    ahead = gitsync.commits_ahead_of_baseline()
    if ahead == 0:
        raise SpiceError(
            "nothing to capture: no local commits ahead of the baseline; "
            "task capture folds an existing loose commit into a task"
        )
    require_no_active_plan_phase_implementation("task capture")
    actor = tw.current_actor()
    if handle is not None:
        if title or project or description or origin:
            raise SpiceError(
                "task capture takes either an existing <handle> or new-task fields "
                "(--title/--project/--description/--origin), not both"
            )
        row = identity.resolve(handle)
        _require_pending(row, "capture")
        _require_manual_claim_allowed(row, actor)
        owner = str(row.get("claim_by") or "")
        if owner and owner != actor:
            raise SpiceError(_claimed_task_capture_recovery_message(row, owner))
        _require_single_active_slot(actor, action="task capture", target=row)
    else:
        _require_single_active_slot(actor, action="task capture")
        if not project:
            raise SpiceError(
                "task capture requires --project when minting a new task; captured "
                "work auto-claims regardless of lifetime, so there is no private "
                "fallback here"
            )
        from spice.tasks import create

        created = create.add_one(
            title=(title or "").strip() or _capture_default_title(),
            description=description,
            project=project,
            priority=priority,
            flow=None,
            tags=[],
            after=[],
            acceptance=[],
            wait=None,
            # Claim below without prepare_for_claim; the loose commits must not
            # be fast-forwarded away before the claim records them.
            claim=False,
            origin=origin,
        )
        row = identity.resolve(created)
    handle_text = identity.render_handle(row)
    # Deliberately skip gitsync.prepare_for_claim: its baseline fast-forward
    # would discard the very loose commits capture exists to preserve.
    do_claim(identity.uuid_of(row), actor, guard_unclaimed=False)
    noun = "commit" if ahead == 1 else "commits"
    captured = f"captured {ahead} loose {noun} into {handle_text}"
    if complete:
        return f"{captured}\n{done(handle_text, validation=validation)}"
    return f'{captured}\nnext: spice task done {handle_text} --validation "..."'


# ---- done / advance -----------------------------------------------------


LEARNING_DIAGNOSTIC_DETAIL_LIMIT = 160


@dataclass(frozen=True)
class _TaskLearningDistillation:
    stored: int = 0
    extracted: int = 0
    skipped: tuple[str, ...] = ()
    reason: str = ""
    detail: str = ""

    def render(self) -> str:
        if self.stored:
            suffix = f"; skipped {len(self.skipped)}" if self.skipped else ""
            return (
                f"learnings: stored {self.stored} accepted "
                f"from {self.extracted} candidate(s){suffix}"
            )
        detail = f": {self.detail}" if self.detail else ""
        return f"learnings: skipped {self.reason}{detail}"


def _publish_meta(
    row: dict[str, Any], actor: str, validation: list[str]
) -> dict[str, str]:
    """Harvest task facts for the programmatic merge commit message."""
    commit_validation = next((v for v in reversed(validation) if v), "")
    return {
        "title": str(row.get("description") or ""),
        "description": str(row.get("task_description") or ""),
        "uuid": str(row.get("uuid") or ""),
        "project": str(row.get("project") or ""),
        "phase": str(row.get("phase") or ""),
        "actor": str(row.get("claim_by") or actor),
        "validation": commit_validation,
    }


def _advance(row: dict[str, Any], *, review_author: str | None = None) -> str:
    uuid = identity.uuid_of(row)
    phases = phases_of(row)
    index = phase_index(row)
    handle = identity.render_handle(row)
    actor = str(row.get("claim_by") or "").strip() or tw.current_actor()
    if index + 1 >= len(phases):
        project = str(row.get("project") or "")
        tw.run([uuid, "done"])
        _record_task_lifecycle_event(uuid, "complete", actor)
        _record_task_lifecycle_event(uuid, "drain", actor)
        _gc_empty_project_task_filters(project)
        return f"completed {handle}"
    nxt = phases[index + 1]
    # One atomic modify: advance the phase, deactivate, and release the claim.
    args = [
        uuid,
        "modify",
        f"phase_i:{index + 1}",
        f"phase:{nxt}",
        "start:",
        *CLAIM_CLEAR,
    ]
    if nxt == "review":
        author = review_author or str(row.get("claim_by") or "") or tw.current_actor()
        args.append(f"review_author:{author}")
    tw.run(args)
    _record_task_lifecycle_event(uuid, "phaseAdvance", actor)
    return f"advanced {handle} -> {nxt}"


def done(
    handle: str | None,
    *,
    validation: list[str],
    judgment: str | None = None,
    notes: list[str] | None = None,
    chain_next: bool = False,
) -> str:
    if not validation:
        raise SpiceError("task done requires --validation")
    tw.require_clean_worktree("task done")
    row = resolve_claim_target(handle, action="done")
    handle = identity.render_handle(row)
    _require_pending(row, "complete")
    actor = tw.current_actor()
    _require_owner(row, actor, "complete")
    _require_bound_quality_gates_clean(row)
    _require_plan_phase_board_populated(row)
    _require_plan_phase_done_has_no_local_commits(row)
    uuid = identity.uuid_of(row)
    # Integrate and publish this agent's work before any task state changes; a
    # real conflict raises here, leaving the task claimed for the agent to fix.
    sync = gitsync.integrate_and_publish(
        identity.render_handle(row),
        meta=_publish_meta(row, actor, validation),
    )
    for note_text in notes or []:
        annotate(uuid, note_text)
    for item in validation:
        annotate(uuid, f"validation: {item}")
    modify = [
        uuid,
        "modify",
        f"validation:{' | '.join(validation)}",
        *sync.uda_args,
    ]
    if judgment:
        modify.append(f"judgment:{judgment}")
    tw.run(modify)
    result = _advance(identity.resolve(handle))
    learning_line = _distill_task_done_learnings(
        row,
        done_at=tw.now_iso(),
        handle_text=identity.render_handle(row),
        repo_root=config.repo_root(),
    ).render()
    next_line = next_task_drain_line()
    if result.endswith(" -> review"):
        next_line = next_task_drain_line(review_assignment=True)
    if not chain_next:
        return "\n".join([result, *sync.notes, learning_line, next_line])
    # Deferred to keep the read-side render -> ops dependency acyclic at import
    # time while reusing exactly the allocator continuation behind `task next`.
    from spice.tasks import render

    return "\n".join([result, *sync.notes, learning_line, render.render_next()])


def _distill_task_done_learnings(
    row: dict[str, Any],
    *,
    done_at: str,
    handle_text: str,
    repo_root: Path,
) -> _TaskLearningDistillation:
    project = str(row.get("project") or "").strip()
    claim_started_at = str(row.get("claim_at") or "").strip()
    thread_id = str(row.get("claim_thread") or "").strip()
    if not project or not claim_started_at or not done_at or not thread_id:
        return _TaskLearningDistillation(reason="missing_claim_metadata")
    try:
        project_stem = config.project_stem(project)
    except SpiceError as exc:
        return _TaskLearningDistillation(
            reason="invalid_project_stem",
            detail=_learning_detail(exc),
        )
    try:
        transcript = session_resolve.resolve_thread_transcript(
            thread_id, repo_root=repo_root
        )
    except SystemExit as exc:
        return _TaskLearningDistillation(
            reason="missing_transcript",
            detail=_learning_detail(exc),
        )
    except (OSError, RuntimeError, SpiceError) as exc:
        return _TaskLearningDistillation(
            reason="missing_transcript",
            detail=_learning_detail(exc),
        )
    try:
        turns = session_records.collect_turns([transcript])
        compactions = session_records.collect_compactions([transcript])
        extracted = session_learnings.extract_learning_candidates_from_task_slice(
            turns,
            compactions,
            claim_started_at=claim_started_at,
            done_at=done_at,
            source_task=handle_text,
            project_stem=project_stem,
        )
    except Exception as exc:
        return _TaskLearningDistillation(
            reason="extract_error",
            detail=_learning_detail(exc),
        )
    if not extracted:
        return _TaskLearningDistillation(reason="no_candidates")
    candidates: list[session_learnings.LearningCandidate] = []
    malformed = 0
    for candidate in extracted:
        try:
            learning = candidate.to_learning_candidate()
            session_learnings.normalize_learning_statement(learning.statement)
        except SpiceError:
            malformed += 1
            continue
        candidates.append(learning)
    if not candidates:
        return _TaskLearningDistillation(
            extracted=len(extracted),
            reason="malformed_candidate",
            detail=f"{malformed} malformed",
        )
    try:
        judged = session_learnings.judge_filter_learning_candidates(candidates)
    except Exception as exc:
        return _TaskLearningDistillation(
            extracted=len(extracted),
            reason="judge_error",
            detail=_learning_detail(exc),
        )
    if not judged.kept:
        return _TaskLearningDistillation(
            extracted=len(extracted),
            skipped=tuple(skip.reason for skip in judged.skipped),
            reason=_learning_skip_reason(judged.skipped),
        )
    try:
        hook_install.materialize_state_gitignore(repo_root)
        confirmed = session_learnings.confirm_learning_candidates(
            repo_root,
            project_stem,
            judged.kept,
        )
    except Exception as exc:
        return _TaskLearningDistillation(
            extracted=len(extracted),
            skipped=tuple(skip.reason for skip in judged.skipped),
            reason="store_error",
            detail=_learning_detail(exc),
        )
    return _TaskLearningDistillation(
        stored=len(confirmed),
        extracted=len(extracted),
        skipped=tuple(skip.reason for skip in judged.skipped),
    )


def _learning_skip_reason(
    skipped: Sequence[session_learnings.LearningJudgeSkip],
) -> str:
    if not skipped:
        return "rejected"
    counts = Counter(skip.reason for skip in skipped)
    return ",".join(
        reason if count == 1 else f"{reason}x{count}"
        for reason, count in sorted(counts.items())
    )


def _learning_detail(exc: BaseException) -> str:
    detail = " ".join(str(exc).split())
    return detail[:LEARNING_DIAGNOSTIC_DETAIL_LIMIT]


def _require_bound_quality_gates_clean(row: dict[str, Any]) -> None:
    """A task tagged ``gate:<key>`` cannot complete while that gate is dirty.

    Completion is bound to the live check, not the prose validation: the metric
    is read by running the gate, so a 'drive to zero' task fails here whenever
    its detector is still nonzero.
    """
    tags = row.get("tags") or []
    if not any(tag.startswith(precommit.GATE_TAG_PREFIX) for tag in tags):
        return
    repo_root = repo_root_from_cwd()
    if repo_root is None:
        raise SpiceError(
            "task is bound to a quality gate but no repo root was found from cwd"
        )
    failures = precommit.quality_gate_failures_for_tags(repo_root, list(tags))
    if failures:
        joined = "\n\n".join(failures)
        raise SpiceError(
            "cannot complete: this task is bound to a quality gate that is not "
            "clean; drive the metric to zero before completing (validation is the "
            f"live check, not a prose claim):\n\n{joined}"
        )


# ---- review -------------------------------------------------------------


def review(
    handle: str | None,
    *,
    finding: str = "clean",
    note: str | None = None,
    then: list[str] | None = None,
    followup: list[str] | None = None,
    creation_surface: str | None = None,
) -> str:
    finding = (finding or "clean").strip()
    if finding.casefold() != "clean" and not then and not followup:
        raise SpiceError(
            "unclean task review requires follow-up tracking; "
            'use --then "title=... | project=... [| acceptance=...]" '
            "or --followup HANDLE"
        )
    tw.require_clean_worktree("task review")
    row = resolve_claim_target(handle, action="review")
    handle = identity.render_handle(row)
    _require_pending(row, "review")
    if str(row.get("phase") or "") != "review":
        raise SpiceError("task review requires a task in the review phase")
    actor = tw.current_actor()
    _require_owner(row, actor, "review")
    uuid = identity.uuid_of(row)
    at = tw.now_iso()
    modify = [
        uuid,
        "modify",
        f"review_by:{actor}",
        f"review_at:{at}",
        f"review_finding:{finding}",
    ]
    if note:
        modify.append(f"review_note:{note}")
    tw.run(modify)
    annotate(uuid, f"review: finding={finding}; by={actor}")
    reviewed_handle = identity.render_handle(row)
    spawned: list[str] = []
    for spec in then or []:
        spawned.append(
            _spawn_followup(
                spec,
                after_uuid=uuid,
                after_handle=reviewed_handle,
                creation_surface=creation_surface,
            )
        )
    linked: list[str] = []
    for followup_handle in followup or []:
        linked.append(
            _link_existing_followup(
                followup_handle, after_uuid=uuid, after_handle=reviewed_handle
            )
        )
    sync = gitsync.integrate_and_publish(
        identity.render_handle(row),
        meta=_publish_meta(row, actor, [note or ""]),
    )
    tw.run([uuid, "modify", *sync.uda_args])
    feedback = reviewfeedback.emit_review_feedback(
        row,
        finding=finding,
        note=note,
        followups=[*spawned, *linked],
        reviewer=actor,
        reviewed_at=at,
    )
    _record_task_lifecycle_event(uuid, "review", actor)
    result = _advance(identity.resolve(handle))
    lines = [f"reviewed {identity.render_handle(row)} {finding}; {result}"]
    lines += [f"spawned {h}" for h in spawned]
    lines += [f"linked {h}" for h in linked]
    if feedback.status != "clean":
        lines.append(feedback.output_line())
    lines.append(next_task_drain_line())
    return "\n".join(lines)


def next_task_drain_line(
    *, review_assignment: bool = False, actor: str | None = None
) -> str:
    contract = _task_continuation_contract(actor)
    if not contract.drain_after_phase_boundary:
        tail = (
            "run spice task next only when explicitly directed to continue "
            "allocator work; capture operator task-creation requests "
            "immediately with a TASK directive that starts on its own line; "
            "when ACKing, write ACK <key>: captured the request. then put TASK "
            "title=... | project=<stem.child> [| acceptance=...] on the next "
            "line using the same task-add batch format; omitted acceptance "
            "with no flow starts in plan, or spice task add before continuing "
            "other work; the captured task inherits "
            "origin=ack:<key> from your ACK (prefer that; set origin= only "
            "when the provenance differs); immediate task capture is not "
            "allocator selection; manual task claims are exceptional and "
            "usually require explicit operator direction"
        )
        if review_assignment:
            return (
                f"next: review assignment pending; {tail}; "
                "self-review only if next assigns it"
            )
        return f"next: phase boundary reached; {tail}"
    tail = (
        "keep working until no allocator-selected work remains or a real blocker exists"
    )
    if review_assignment:
        return (
            "next: YOU ARE NOT DONE. Run spice task next for reviewer assignment; "
            "self-review only if next assigns it; "
            f"{tail}"
        )
    return f"next: YOU ARE NOT DONE. Run spice task next; {tail}"


def _spawn_followup(
    spec: str,
    *,
    after_uuid: str,
    after_handle: str,
    creation_surface: str | None = None,
) -> str:
    from spice.tasks import create

    request = create.parse_task_batch_request(spec, require_project=False)
    return create.add_one(
        title=request.title,
        description=request.description,
        project=request.project,
        priority=request.priority,
        flow=list(request.flow) or None,
        tags=list(request.tags),
        after=list(request.after),
        acceptance=list(request.acceptance),
        wait=None,
        claim=False,
        deferred=request.deferred,
        due=request.due,
        # A review follow-up descends from the reviewed task; an explicit
        # origin= field in the spec wins.
        origin=request.origin or f"task:{after_handle}",
        extra=[f"depends:{after_uuid}"],
        creation_surface=creation_surface,
    )


def _link_existing_followup(handle: str, *, after_uuid: str, after_handle: str) -> str:
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    if uuid == after_uuid:
        raise SpiceError("a review follow-up cannot be the reviewed task itself")
    try:
        tw.run([uuid, "modify", f"depends:{after_uuid}"])
    except SpiceError as exc:
        raise SpiceError(
            f"could not link existing review follow-up {identity.render_handle(row)} "
            "(would it create a cycle?)"
        ) from exc
    annotate(uuid, f"review follow-up depends on {after_handle}")
    return identity.render_handle(row)


# ---- oops / note / depends / delete --------------------------------------


def oops(
    text: str,
    *,
    description: str = "",
    severity: str = "medium",
    kind: str = "",
    surface: str = "",
    command: str = "",
    workaround: str = "",
    origin: str = "",
    tags: list[str] | None = None,
) -> str:
    severity = config.map_severity(severity)
    # Identity is the project string: a kind files under the .oops.<kind>
    # child board, and severity rides native priority alone.
    kind = kind.strip().lower()
    project = f"{config.OOPS_PROJECT}.{kind}" if kind else config.OOPS_PROJECT
    from spice.tasks import create

    handle = create.add_one(
        title=text,
        description=description or None,
        project=project,
        priority=config.SEVERITY_PRIORITY[severity],
        flow=None,
        tags=list(tags or []),
        after=[],
        acceptance=[],
        wait=config.OOPS_WAIT,
        claim=False,
        origin=origin or None,
        system_project=True,
    )
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    for label, value in (
        ("surface", surface),
        ("command", command),
        ("workaround", workaround),
    ):
        if value:
            annotate(uuid, f"{label}: {value}")
    return f"oops {handle} [{severity}]"


def note(handle: str, text: str) -> str:
    row = identity.resolve(handle)
    annotate(identity.uuid_of(row), text)
    return f"noted {identity.render_handle(row)}"


def depends(handle: str, after: list[str]) -> str:
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    for dep in after:
        dep_row = identity.resolve(dep)
        dep_uuid = identity.uuid_of(dep_row)
        if dep_uuid == uuid:
            raise SpiceError("a task cannot depend on itself")
        try:
            tw.run([uuid, "modify", f"depends:{dep_uuid}"])
        except SpiceError as exc:
            raise SpiceError(
                f"could not add dependency on {identity.render_handle(dep_row)} "
                "(would it create a cycle?)"
            ) from exc
        annotate(uuid, f"depends: {identity.render_handle(dep_row)}")
    return identity.render_handle(row)


def undepends(handle: str, after: list[str]) -> str:
    row = identity.resolve(handle)
    uuid = identity.uuid_of(row)
    rendered = identity.render_handle(row)
    existing = set(_dependency_uuids(row))
    annotations = _annotation_descriptions(row)
    for dep in dict.fromkeys(after):
        dep_row = identity.resolve(dep)
        dep_uuid = identity.uuid_of(dep_row)
        rendered_dep = identity.render_handle(dep_row)
        if dep_uuid not in existing:
            raise SpiceError(f"{rendered} does not depend on {rendered_dep}")
        tw.run([uuid, "modify", f"depends:-{dep_uuid}"])
        note = f"depends: {rendered_dep}"
        if note in annotations:
            denotate(uuid, note)
    return rendered


def _annotation_descriptions(row: dict[str, Any]) -> set[str]:
    annotations = row.get("annotations") or []
    if not isinstance(annotations, list):
        return set()
    return {
        str(item.get("description") or "")
        for item in annotations
        if isinstance(item, dict)
    }


def _dependency_uuids(row: dict[str, Any]) -> list[str]:
    raw = row.get("depends") or []
    if isinstance(raw, str):
        return [raw] if raw else []
    return [str(item) for item in raw if str(item)]


def _pending_plan_child_rows(row: dict[str, Any]) -> list[dict[str, Any]]:
    """Pending board rows connected to a plan task by native dependencies."""
    plan_uuid = identity.uuid_of(row)
    children: dict[str, dict[str, Any]] = {}
    for dep_uuid in _dependency_uuids(row):
        rows = tw.export([dep_uuid])
        if rows and str(rows[0].get("status") or "") == "pending":
            children[identity.uuid_of(rows[0])] = rows[0]
    for candidate in tw.export(["status:pending"]):
        candidate_uuid = identity.uuid_of(candidate)
        if candidate_uuid == plan_uuid:
            continue
        if plan_uuid in _dependency_uuids(candidate):
            children[candidate_uuid] = candidate
    return list(children.values())


def _require_plan_phase_board_populated(row: dict[str, Any]) -> None:
    if str(row.get("phase") or "") != "plan":
        return
    plan_only = phases_of(row) == ["plan"]
    if not plan_only and str(row.get("acceptance") or "").strip():
        return
    handle = identity.render_handle(row)
    children = _pending_plan_child_rows(row)
    if any(str(child.get("acceptance") or "").strip() for child in children):
        return
    if plan_only:
        raise SpiceError(
            f"cannot complete plan-only flow for {handle}: connect at least one "
            "pending child task with acceptance"
        )
    raise SpiceError(
        f"cannot advance plan phase for {handle}: add acceptance to the current "
        "task or connect at least one pending child task with acceptance"
    )


def delete(handle: str, reason: str, *, force_claimed: bool = False) -> str:
    row = identity.resolve(handle)
    _require_pending(row, "delete")
    live_claim = _live_claim(row)
    rendered = identity.render_handle(row)
    if live_claim and not force_claimed:
        raise SpiceError(
            f"cannot delete {rendered}: live claim held by "
            f"{_live_claim_text(live_claim)}; rerun with --force-claimed to "
            "override"
        )
    uuid = identity.uuid_of(row)
    project = str(row.get("project") or "")
    if live_claim:
        annotate(uuid, f"forced delete of live claim: {_live_claim_text(live_claim)}")
    annotate(uuid, f"deleted: {reason}")
    tw.run([uuid, "modify", f"delete_reason:{reason}"])
    tw.run([uuid, "delete"])
    _gc_empty_project_task_filters(project)
    if live_claim:
        return (
            f"warning: deleted {rendered} despite live claim "
            f"{_live_claim_text(live_claim)}\n{rendered}"
        )
    return rendered
