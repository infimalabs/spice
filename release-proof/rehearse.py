#!/usr/bin/env python3
"""Execute the hermetic release gates and retain the exact proved artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile
from pathlib import Path
from typing import Any

SCRIPT_DIRECTORY = Path(__file__).resolve().parent
if str(SCRIPT_DIRECTORY) not in sys.path:
    sys.path.insert(0, str(SCRIPT_DIRECTORY))

from evidence import (  # noqa: E402
    FAILURE_DIRNAME,
    FailureArtifactStore,
    failure_policy_payload,
    parse_pytest_counts,
    redact_text,
)

SCHEMA_VERSION = 1
EXIT_FAILURE = 2
HASH_CHUNK_BYTES = 1024 * 1024
PYTHON_GATE_COMMAND = ("uv", "run", "--locked", "pytest")
RUFF_GATE_COMMAND = ("uv", "run", "--locked", "ruff", "check", ".")
TOOLCHAIN_DECLARATION_PATH = "release-proof/toolchain.json"
PACKAGING_TOOLCHAIN_GROUP = "release"
PACKAGING_TOOLCHAIN_PINS = ("build", "setuptools", "twine", "wheel")
BROWSER_GATE_COMMAND = ("node", "tests/browser/run_release_smokes.js")
MUTATION_GATE_COMMAND = (
    "uv",
    "run",
    "--locked",
    "spice",
    "study",
    "mutations",
    "spice/config/layers.py",
    "--test",
    "tests/test_configlayer.py",
    "--max-mutants",
    "20",
    "--timeout",
    "30",
    "--ratchet",
    "tests/mutation-ratchet.json",
    "--json",
)
PLAYWRIGHT_CONFIG_ENV = "SPICE_PLAYWRIGHT_MCP_CONFIG"  # env-policy: allow
RECEIPT_NAME = "release-proof.json"
BROWSER_REPORT_NAME = "browser-scenarios.json"
MUTATION_COUNT_FIELDS = ("killed", "mutants", "score", "survived", "timed_out")
CHECKS = (
    "packaging-toolchain",
    "python",
    "ruff",
    "browser-release-manifest",
    "deterministic-mutation-cohort",
    "build-sdist",
    "build-wheel",
    "metadata",
    "isolated-install",
    "installed-imports",
    "installed-console",
    "sdist-rebuild",
    "wheel-member-content",
    "clean-worktree",
)


class RehearsalError(RuntimeError):
    """The release proof could not establish one required invariant."""


def _run(
    command: list[str] | tuple[str, ...],
    *,
    cwd: Path,
    capture: bool = False,
    env: dict[str, str] | None = None,
    failures: FailureArtifactStore | None = None,
    gate: str = "command",
) -> subprocess.CompletedProcess[str]:
    argv = [str(part) for part in command]
    print(f"+ {shlex.join(argv)}", flush=True)
    completed = subprocess.run(
        argv,
        check=False,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if completed.returncode != 0:
        environment = (
            env if env is not None else dict(os.environ)  # env-policy: allow
        )
        diagnostic = (
            failures.record(
                gate,
                argv,
                completed.returncode,
                completed.stdout,
                completed.stderr,
                environment=environment,
            )
            if failures is not None
            else None
        )
        safe_stdout = redact_text(completed.stdout, environment).strip()
        safe_stderr = redact_text(completed.stderr, environment).strip()
        if safe_stdout:
            print(safe_stdout, file=sys.stderr)
        if safe_stderr:
            print(safe_stderr, file=sys.stderr)
        suffix = f"; diagnostic={diagnostic}" if diagnostic is not None else ""
        raise RehearsalError(
            f"{gate} failed with exit code {completed.returncode}{suffix}"
        )
    if not capture and completed.stdout:
        print(completed.stdout, end="")
    if completed.stderr:
        print(completed.stderr, end="", file=sys.stderr)
    return completed


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(HASH_CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def packaging_command(project: Path, *arguments: str) -> tuple[str, ...]:
    """Run a pinned packaging tool from the project's locked environment.

    The artifact steps deliberately work outside the source tree, so the
    project is named rather than discovered: ``--project`` steers discovery
    only and leaves the working directory where the caller put it.
    """
    return (
        "uv",
        "run",
        "--project",
        str(project),
        "--locked",
        "--group",
        PACKAGING_TOOLCHAIN_GROUP,
        "python",
        *arguments,
    )


def _declared_packaging_pins(root: Path) -> dict[str, str]:
    declaration = json.loads(
        (root / TOOLCHAIN_DECLARATION_PATH).read_text(encoding="utf-8")
    )
    pinned = declaration["pinned"]
    return {name: str(pinned[name]) for name in PACKAGING_TOOLCHAIN_PINS}


def verify_packaging_toolchain(
    root: Path,
    failures: FailureArtifactStore | None = None,
) -> dict[str, str]:
    """Resolve the pinned packaging toolchain before any gate spends time.

    Every artifact-chain step needs build, setuptools, twine and wheel, and
    they arrive from the same locked environment the source gates already use.
    Proving that up front turns a missing or drifted dependency into a named
    failure in seconds, instead of one that lands only after the test, browser
    and mutation gates have already run.
    """
    expected = _declared_packaging_pins(root)
    if shutil.which("uv") is None:
        raise RehearsalError(
            "the release rehearsal drives its packaging toolchain through uv, "
            f"which is not on PATH; install uv and rerun (declared pins: "
            f"{json.dumps(expected, sort_keys=True)})"
        )
    names = ", ".join(repr(name) for name in PACKAGING_TOOLCHAIN_PINS)
    expression = (
        "import json; from importlib.metadata import version; "
        f"print(json.dumps({{name: version(name) for name in ({names},)}}))"
    )
    completed = _run(
        packaging_command(root, "-c", expression),
        cwd=root,
        capture=True,
        failures=failures,
        gate="packaging-toolchain",
    )
    resolved = {
        name: str(value) for name, value in json.loads(completed.stdout).items()
    }
    if resolved != expected:
        drifted = sorted(
            name for name in expected if resolved.get(name) != expected[name]
        )
        raise RehearsalError(
            "packaging toolchain differs from "
            f"{TOOLCHAIN_DECLARATION_PATH} for {', '.join(drifted)}:\n"
            + json.dumps(
                {"declared": expected, "resolved": resolved}, indent=2, sort_keys=True
            )
        )
    return resolved


def _project_version(root: Path) -> str:
    project = tomllib.loads((root / "pyproject.toml").read_text(encoding="utf-8"))[
        "project"
    ]
    return str(project["version"])


def _playwright_config(root: Path, failures: FailureArtifactStore) -> Path:
    expression = (
        "from pathlib import Path; "
        "from spice.agent.driver import write_playwright_mcp_config; "
        "print(write_playwright_mcp_config(Path.cwd()))"
    )
    result = _run(
        ["uv", "run", "--locked", "python", "-c", expression],
        cwd=root,
        capture=True,
        failures=failures,
        gate="browser-config",
    )
    path = Path(result.stdout.strip())
    if not path.is_file():
        raise RehearsalError(f"Playwright config was not materialized: {path}")
    return path


def _run_source_gates(
    root: Path,
    scratch: Path,
    failures: FailureArtifactStore,
) -> dict[str, object]:
    python = _run(
        PYTHON_GATE_COMMAND,
        cwd=root,
        capture=True,
        failures=failures,
        gate="python",
    )
    try:
        python_counts = parse_pytest_counts(python.stdout)
    except ValueError as exc:
        raise RehearsalError(str(exc)) from exc
    print(python.stdout, end="")
    _run(RUFF_GATE_COMMAND, cwd=root, failures=failures, gate="ruff")
    browser_env = dict(os.environ)  # env-policy: allow
    browser_env[PLAYWRIGHT_CONFIG_ENV] = str(_playwright_config(root, failures))
    browser_report_path = scratch / BROWSER_REPORT_NAME
    browser_env["SPICE_RELEASE_BROWSER_REPORT"] = str(  # env-policy: allow
        browser_report_path
    )
    _run(
        BROWSER_GATE_COMMAND,
        cwd=root,
        env=browser_env,
        failures=failures,
        gate="browser",
    )
    browser = _load_browser_report(browser_report_path)
    mutation = _run(
        MUTATION_GATE_COMMAND,
        cwd=root,
        capture=True,
        failures=failures,
        gate="mutation",
    )
    return {
        "python": python_counts,
        "ruff": {"passed": True},
        "browser": browser,
        "mutation": verify_mutation_cohort(root, mutation.stdout),
    }


def _load_browser_report(path: Path) -> dict[str, object]:
    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise RehearsalError(
            f"could not read browser scenario report {path}: {exc}"
        ) from exc
    if not isinstance(payload, dict) or payload.get("schemaVersion") != 1:
        raise RehearsalError(f"invalid browser scenario report: {path}")
    counts = payload.get("counts")
    scenarios = payload.get("scenarios")
    external = payload.get("externalState")
    if not isinstance(counts, dict) or not isinstance(scenarios, list):
        raise RehearsalError(f"incomplete browser scenario report: {path}")
    if not isinstance(external, list) or counts.get("failed") != 0:
        raise RehearsalError(f"browser scenario report records failures: {path}")
    return payload


def verify_mutation_cohort(root: Path, output: str) -> dict[str, dict[str, object]]:
    payload = json.loads(output)
    reports = payload.get("reports")
    if not isinstance(reports, list):
        raise RehearsalError("mutation proof did not return a reports list")
    actual: dict[str, dict[str, object]] = {}
    for raw in reports:
        if not isinstance(raw, dict) or not isinstance(raw.get("path"), str):
            raise RehearsalError("mutation proof returned an invalid report")
        actual[str(raw["path"])] = {
            field: raw.get(field) for field in MUTATION_COUNT_FIELDS
        }
    ratchet = json.loads(
        (root / "tests/mutation-ratchet.json").read_text(encoding="utf-8")
    )
    modules = ratchet.get("modules")
    if not isinstance(modules, dict):
        raise RehearsalError("mutation ratchet has no modules map")
    expected = {
        str(path): {field: values.get(field) for field in MUTATION_COUNT_FIELDS}
        for path, values in modules.items()
        if isinstance(values, dict)
    }
    regressions = payload.get("ratchetRegressions")
    if (actual, regressions) != (expected, []):
        raise RehearsalError(
            "deterministic mutation cohort differs from its standing ratchet:\n"
            + json.dumps(
                {
                    "actual": actual,
                    "expected": expected,
                    "ratchetRegressions": regressions,
                },
                indent=2,
                sort_keys=True,
            )
        )
    return actual


def _materialize_committed_source(root: Path, scratch: Path) -> Path:
    archive = scratch / "source.tar"
    source = scratch / "source"
    _run(
        [
            "git",
            "archive",
            "--format=tar",
            "--output",
            str(archive),
            "HEAD",
        ],
        cwd=root,
    )
    source.mkdir()
    with tarfile.open(archive, "r:") as source_archive:
        source_archive.extractall(source, filter="data")
    return source


def _build_canonical_artifacts(
    project: Path,
    root: Path,
    artifact_dir: Path,
    version: str,
    failures: FailureArtifactStore | None = None,
) -> tuple[Path, Path]:
    artifact_dir.mkdir(parents=True, exist_ok=True)
    _run(
        packaging_command(
            project,
            "-m",
            "build",
            "--no-isolation",
            "--sdist",
            "--outdir",
            str(artifact_dir),
            str(root),
        ),
        cwd=artifact_dir,
        failures=failures,
        gate="build-sdist",
    )
    _run(
        packaging_command(
            project,
            "-m",
            "build",
            "--no-isolation",
            "--wheel",
            "--outdir",
            str(artifact_dir),
            str(root),
        ),
        cwd=artifact_dir,
        failures=failures,
        gate="build-wheel",
    )
    sdist = artifact_dir / f"spice_harness-{version}.tar.gz"
    wheel = artifact_dir / f"spice_harness-{version}-py3-none-any.whl"
    resolved = tuple(sorted(path.name for path in artifact_dir.iterdir()))
    expected = tuple(sorted((sdist.name, wheel.name)))
    if resolved != expected:
        raise RehearsalError(
            f"canonical build artifacts differ: expected={expected!r} "
            f"resolved={resolved!r}"
        )
    _run(
        packaging_command(project, "-m", "twine", "check", str(sdist), str(wheel)),
        cwd=artifact_dir,
        failures=failures,
        gate="metadata",
    )
    return sdist, wheel


def _isolated_environment() -> dict[str, str]:
    environment = dict(os.environ)  # env-policy: allow
    for name in ("PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV"):
        environment.pop(name, None)
    return environment


def _validate_installed_wheel(
    root: Path,
    wheel: Path,
    version: str,
    scratch: Path,
    failures: FailureArtifactStore | None = None,
) -> None:
    venv = scratch / "installed"
    python = venv / "bin" / "python"
    spice = venv / "bin" / "spice"
    _run(
        [sys.executable, "-m", "venv", str(venv)],
        cwd=scratch,
        failures=failures,
        gate="isolated-venv",
    )
    _run(
        ["uv", "pip", "install", "--python", str(python), str(wheel)],
        cwd=scratch,
        failures=failures,
        gate="isolated-install",
    )
    probe = (
        "from importlib.metadata import version; "
        "from importlib.resources import files; "
        "from spice.config import layers; "
        f"assert version('spice-harness') == {version!r}; "
        "assert files('spice').joinpath('spice.toml').is_file(); "
        "print(layers.__file__)"
    )
    environment = _isolated_environment()
    _run(
        [str(python), "-I", "-c", probe],
        cwd=scratch,
        env=environment,
        failures=failures,
        gate="installed-imports",
    )
    for arguments in (
        ("--version",),
        ("task", "--help"),
        ("session", "--help"),
    ):
        _run(
            [str(spice), *arguments],
            cwd=scratch,
            env=environment,
            failures=failures,
            gate="installed-console",
        )
    if (
        root
        in Path(
            _run(
                [str(python), "-I", "-c", "import spice; print(spice.__path__[0])"],
                cwd=scratch,
                capture=True,
                env=environment,
                failures=failures,
                gate="installed-import-origin",
            ).stdout.strip()
        ).parents
    ):
        raise RehearsalError("isolated import resolved back into the source worktree")


def _extract_sdist(sdist: Path, destination: Path, version: str) -> Path:
    destination.mkdir()
    with tarfile.open(sdist, "r:gz") as archive:
        archive.extractall(destination, filter="data")
    source = destination / f"spice_harness-{version}"
    if not source.is_dir():
        raise RehearsalError(f"sdist did not contain its canonical root: {source}")
    return source


def _rebuild_wheel_from_sdist(
    project: Path,
    sdist: Path,
    version: str,
    scratch: Path,
    failures: FailureArtifactStore | None = None,
) -> Path:
    source = _extract_sdist(sdist, scratch / "sdist", version)
    rebuilt_dir = scratch / "rebuilt"
    rebuilt_dir.mkdir()
    _run(
        packaging_command(
            project,
            "-m",
            "build",
            "--no-isolation",
            "--wheel",
            "--outdir",
            str(rebuilt_dir),
            str(source),
        ),
        cwd=rebuilt_dir,
        failures=failures,
        gate="sdist-rebuild",
    )
    wheel = rebuilt_dir / f"spice_harness-{version}-py3-none-any.whl"
    if not wheel.is_file():
        raise RehearsalError(f"sdist rebuild did not produce {wheel.name}")
    return wheel


def _wheel_member_hashes(path: Path) -> dict[str, str]:
    with zipfile.ZipFile(path) as archive:
        return {
            info.filename: hashlib.sha256(archive.read(info)).hexdigest()
            for info in archive.infolist()
        }


def wheel_member_mismatches(canonical: Path, rebuilt: Path) -> list[dict[str, str]]:
    """Return every exact member-level delta, ignoring only ZIP container bytes."""
    expected = _wheel_member_hashes(canonical)
    resolved = _wheel_member_hashes(rebuilt)
    mismatches: list[dict[str, str]] = []
    for name in sorted(expected.keys() - resolved.keys()):
        mismatches.append(
            {
                "kind": "missing-from-rebuilt",
                "member": name,
                "canonical_sha256": expected[name],
            }
        )
    for name in sorted(resolved.keys() - expected.keys()):
        mismatches.append(
            {
                "kind": "extra-in-rebuilt",
                "member": name,
                "rebuilt_sha256": resolved[name],
            }
        )
    for name in sorted(expected.keys() & resolved.keys()):
        if expected[name] != resolved[name]:
            mismatches.append(
                {
                    "kind": "content-changed",
                    "member": name,
                    "canonical_sha256": expected[name],
                    "rebuilt_sha256": resolved[name],
                }
            )
    return mismatches


def _load_git_private_json(root: Path, name: str) -> dict[str, Any]:
    path = root / ".git" / name
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise RehearsalError(f"invalid Git-private proof record: {path}")
    return payload


def _write_receipt(path: Path, payload: dict[str, object]) -> None:
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    temporary.replace(path)


def rehearse(root: Path, artifact_dir: Path) -> dict[str, object]:
    root = root.resolve(strict=True)
    artifact_dir = artifact_dir.resolve()
    artifact_dir.mkdir(parents=True, exist_ok=False)
    failures = FailureArtifactStore(artifact_dir)
    packaging = verify_packaging_toolchain(root, failures)
    version = _project_version(root)
    with tempfile.TemporaryDirectory(prefix="spice-release-rehearsal-") as raw:
        scratch = Path(raw)
        gate_evidence = _run_source_gates(root, scratch, failures)
        source = _materialize_committed_source(root, scratch)
        sdist, wheel = _build_canonical_artifacts(
            root, source, artifact_dir, version, failures
        )
        _validate_installed_wheel(root, wheel, version, scratch, failures)
        rebuilt = _rebuild_wheel_from_sdist(root, sdist, version, scratch, failures)
        mismatches = wheel_member_mismatches(wheel, rebuilt)
        if mismatches:
            raise RehearsalError(
                "sdist-rebuilt wheel member mismatch:\n"
                + json.dumps(mismatches, indent=2, sort_keys=True)
            )
        rebuilt_sha256 = _sha256(rebuilt)
        rebuilt_member_count = len(_wheel_member_hashes(rebuilt))
    status = _run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"],
        cwd=root,
        capture=True,
    ).stdout.strip()
    if status:
        raise RehearsalError(f"release rehearsal left a dirty worktree:\n{status}")
    sdist_sha256 = _sha256(sdist)
    wheel_sha256 = _sha256(wheel)
    receipt: dict[str, object] = {
        "schema_version": SCHEMA_VERSION,
        "version": version,
        "claim_boundary": {
            "operating_system": "linux",
            "host_native_companion": "release-proof-macos.json",
            "host_native_checks": ["kqueue-or-fsevents", "appearance", "speech"],
        },
        "source_identity": _load_git_private_json(
            root, "release-proof-identities.json"
        ),
        "toolchain": _load_git_private_json(root, "release-proof-toolchain.json"),
        "packaging_toolchain": packaging,
        "tests": {
            "python": gate_evidence["python"],
            "ruff": gate_evidence["ruff"],
        },
        "browser": gate_evidence["browser"],
        "mutation": gate_evidence["mutation"],
        "artifacts": {
            "sdist": {
                "filename": sdist.name,
                "bytes": sdist.stat().st_size,
                "sha256": sdist_sha256,
            },
            "wheel": {
                "filename": wheel.name,
                "bytes": wheel.stat().st_size,
                "sha256": wheel_sha256,
            },
            "installed_wheel_sha256": wheel_sha256,
            "sdist_rebuilt_from_sha256": sdist_sha256,
        },
        "artifact_rehearsal": {
            "checks": list(CHECKS),
            "installed_wheel_sha256": wheel_sha256,
            "sdist_rebuilt_from_sha256": sdist_sha256,
        },
        "content_comparison": {
            "canonical_members": len(_wheel_member_hashes(wheel)),
            "rebuilt_members": rebuilt_member_count,
            "mismatches": [],
            "rebuilt_wheel_sha256": rebuilt_sha256,
            "outer_archive_reproducibility": "deferred",
        },
        "failure_diagnostics": failure_policy_payload(),
    }
    _write_receipt(artifact_dir / RECEIPT_NAME, receipt)
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", required=True, type=Path)
    arguments = parser.parse_args()
    try:
        receipt = rehearse(Path(__file__).resolve().parent.parent, arguments.artifacts)
    except (OSError, RehearsalError, ValueError) as exc:
        safe_error = redact_text(str(exc), dict(os.environ))  # env-policy: allow
        failure_dir = arguments.artifacts / FAILURE_DIRNAME
        if not failure_dir.is_dir() or not tuple(failure_dir.glob("*.log")):
            FailureArtifactStore(arguments.artifacts).record(
                "rehearsal",
                [sys.executable, str(Path(__file__).resolve())],
                EXIT_FAILURE,
                "",
                safe_error,
            )
        print(f"release-proof rehearsal: {safe_error}", file=sys.stderr)
        return EXIT_FAILURE
    print(json.dumps(receipt, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
