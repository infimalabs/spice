"""`spice doctor` — one rolled-up verdict across the subsystem doctors."""

from __future__ import annotations

import argparse
import subprocess

from spice import doctor as topdoctor
from spice.cli.parser import build_parser
from spice.hooks.doctor import DoctorCheck, DoctorReport
from spice.tasks import render


def _env_report(repo_root, *, failed):
    status = "fail" if failed else "ok"
    check = DoctorCheck(
        name="git.clean", status=status, detail="detail", command="git status --short"
    )
    return DoctorReport(repo_root=repo_root, checks=[check], fixes=[])


def _patch(monkeypatch, tmp_path, *, env_failed, task_problems, fix_seen=None):
    subprocess.run(["git", "init", "-q", "-b", "main"], cwd=tmp_path, check=True)
    monkeypatch.setattr(topdoctor, "require_repo_root", lambda: tmp_path)

    def fake_env(repo_root, *, fix=False):
        if fix_seen is not None:
            fix_seen.append(fix)
        report = _env_report(repo_root, failed=env_failed)
        return report.render(), report.failed

    monkeypatch.setattr(
        topdoctor,
        "_environment_doctor_result",
        fake_env,
    )
    monkeypatch.setattr(
        topdoctor,
        "_render_task_doctor_report",
        lambda: ("task readout", list(task_problems)),
    )


def test_doctor_parser_wires_fix_flag_to_handler():
    args = build_parser().parse_args(["doctor", "--fix"])
    assert args.func is topdoctor.handle_doctor
    assert args.fix is True


def test_doctor_rolls_up_ok_when_every_subsystem_is_clean(
    tmp_path, monkeypatch, capsys
):
    _patch(monkeypatch, tmp_path, env_failed=False, task_problems=[])

    code = topdoctor.handle_doctor(argparse.Namespace(fix=False))

    out = capsys.readouterr().out
    assert code == 0
    assert out.strip().endswith("doctor ok")
    # Both subsystem readouts are surfaced under the aggregate.
    assert "spice task doctor" in out and "task readout" in out


def test_doctor_fails_when_environment_doctor_fails(tmp_path, monkeypatch, capsys):
    _patch(monkeypatch, tmp_path, env_failed=True, task_problems=[])

    code = topdoctor.handle_doctor(argparse.Namespace(fix=False))

    assert code == 1
    assert capsys.readouterr().out.strip().endswith("doctor FAIL")


def test_doctor_fails_when_task_doctor_reports_problems(tmp_path, monkeypatch, capsys):
    _patch(
        monkeypatch,
        tmp_path,
        env_failed=False,
        task_problems=["actor x has 2 active claims"],
    )

    code = topdoctor.handle_doctor(argparse.Namespace(fix=False))

    assert code == 1
    assert capsys.readouterr().out.strip().endswith("doctor FAIL")


def test_doctor_forwards_fix_to_the_environment_doctor(tmp_path, monkeypatch):
    seen: list[bool] = []
    _patch(monkeypatch, tmp_path, env_failed=False, task_problems=[], fix_seen=seen)

    topdoctor.handle_doctor(argparse.Namespace(fix=True))

    assert seen == [True]


def test_render_doctor_text_matches_the_report_body(monkeypatch):
    # render_doctor keeps its string contract by dropping the problem list that
    # render_doctor_report adds for the aggregate to roll up.
    monkeypatch.setattr(render.tw, "export", lambda *a, **k: [])
    monkeypatch.setattr(render.alloc, "stale_rows", lambda *a, **k: [])

    text, problems = render.render_doctor_report()

    assert problems == []
    assert render.render_doctor() == text
    assert "ok: no problems found" in text
