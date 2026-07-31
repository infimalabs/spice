"""Failure-path diagnostics for the checkout-local pytest runner."""

from __future__ import annotations

import sys
from collections.abc import Sequence
from pathlib import Path

import pytest

from spice.hooks import devpytest


PytestResponse = tuple[int, str, str]


def _bind_checkout_venv(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    repo_root = tmp_path / "repo"
    (repo_root / ".venv").mkdir(parents=True)
    monkeypatch.setattr(devpytest.sys, "prefix", str(repo_root / ".venv"))
    return repo_root


def _stub_pytest_main(
    monkeypatch: pytest.MonkeyPatch,
    responses: Sequence[PytestResponse],
) -> list[list[str]]:
    calls: list[list[str]] = []
    remaining = iter(responses)

    def fake_main(args: list[str]) -> int:
        calls.append(list(args))
        code, stdout, stderr = next(remaining)
        sys.stdout.write(stdout)
        sys.stderr.write(stderr)
        return code

    monkeypatch.setattr(pytest, "main", fake_main)
    return calls


@pytest.mark.parametrize(
    ("pytest_args", "missing_selector"),
    [
        (["-q", "tests/does_not_exist_probe.py"], "tests/does_not_exist_probe.py"),
        (
            [
                "-q",
                "tests/test_toolprocesspolicy.py",
                "tests/does_not_exist_probe.py",
            ],
            "tests/does_not_exist_probe.py",
        ),
        (
            ["-q", "tests/test_toolprocesspolicy.py::no_such_test"],
            "tests/test_toolprocesspolicy.py::no_such_test",
        ),
    ],
)
def test_not_found_selector_replays_pytest_diagnostic_and_exits_usage_error(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    pytest_args: list[str],
    missing_selector: str,
) -> None:
    repo_root = _bind_checkout_venv(tmp_path, monkeypatch)
    calls = _stub_pytest_main(
        monkeypatch,
        [
            (int(pytest.ExitCode.NO_TESTS_COLLECTED), "no tests ran\n", ""),
            (
                int(pytest.ExitCode.USAGE_ERROR),
                "no tests collected\n",
                f"ERROR: not found: {missing_selector}\n",
            ),
        ],
    )

    code = devpytest.run_checkout_pytest(repo_root, pytest_args)

    captured = capsys.readouterr()
    assert code == int(pytest.ExitCode.USAGE_ERROR)
    assert captured.out == "no tests ran\nno tests collected\n"
    assert missing_selector in captured.err
    assert calls == [
        pytest_args,
        [*pytest_args, "-n", "0", "--collect-only"],
    ]


def test_diagnostic_flags_precede_pytest_option_terminator(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    repo_root = _bind_checkout_venv(tmp_path, monkeypatch)
    pytest_args = ["-q", "--", "tests/does_not_exist_probe.py"]
    calls = _stub_pytest_main(
        monkeypatch,
        [
            (int(pytest.ExitCode.NO_TESTS_COLLECTED), "no tests ran\n", ""),
            (
                int(pytest.ExitCode.USAGE_ERROR),
                "no tests collected\n",
                "ERROR: file or directory not found: tests/does_not_exist_probe.py\n",
            ),
        ],
    )

    code = devpytest.run_checkout_pytest(repo_root, pytest_args)

    captured = capsys.readouterr()
    assert code == int(pytest.ExitCode.USAGE_ERROR)
    assert "tests/does_not_exist_probe.py" in captured.err
    assert calls == [
        pytest_args,
        [
            "-q",
            "-n",
            "0",
            "--collect-only",
            "--",
            "tests/does_not_exist_probe.py",
        ],
    ]


def test_empty_k_selection_keeps_original_exit_and_discards_diagnostic_probe(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    repo_root = _bind_checkout_venv(tmp_path, monkeypatch)
    pytest_args = [
        "-q",
        "tests/test_toolprocesspolicy.py",
        "-k",
        "definitely_no_test_matches",
    ]
    calls = _stub_pytest_main(
        monkeypatch,
        [
            (int(pytest.ExitCode.NO_TESTS_COLLECTED), "no tests ran\n", ""),
            (
                int(pytest.ExitCode.NO_TESTS_COLLECTED),
                "no tests collected (17 deselected)\n",
                "",
            ),
        ],
    )

    code = devpytest.run_checkout_pytest(repo_root, pytest_args)

    captured = capsys.readouterr()
    assert code == int(pytest.ExitCode.NO_TESTS_COLLECTED)
    assert captured.out == "no tests ran\n"
    assert captured.err == ""
    assert calls == [
        pytest_args,
        [*pytest_args, "-n", "0", "--collect-only"],
    ]


@pytest.mark.parametrize(
    "pytest_args",
    [
        ["-q", "tests"],
        [
            "-q",
            "tests/test_toolprocesspolicy.py"
            "::test_direct_subprocess_seams_match_the_explicit_policy_catalog",
        ],
        ["-q", "-k", "tool_process", "-p", "no:warnings", "--dist=loadfile"],
    ],
)
def test_clean_run_preserves_selectors_output_and_exit_without_diagnostic_probe(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
    pytest_args: list[str],
) -> None:
    repo_root = _bind_checkout_venv(tmp_path, monkeypatch)
    calls = _stub_pytest_main(
        monkeypatch,
        [(int(pytest.ExitCode.OK), "1 passed\n", "")],
    )

    code = devpytest.run_checkout_pytest(repo_root, pytest_args)

    captured = capsys.readouterr()
    assert code == int(pytest.ExitCode.OK)
    assert captured.out == "1 passed\n"
    assert captured.err == ""
    assert calls == [pytest_args]
