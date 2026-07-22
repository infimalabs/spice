#!/usr/bin/env python3
"""Execute the hermetic release gates and retain the exact proved artifacts."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shlex
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1
EXIT_FAILURE = 2
HASH_CHUNK_BYTES = 1024 * 1024
PYTHON_GATE_COMMAND = ("uv", "run", "--locked", "pytest")
RUFF_GATE_COMMAND = ("uv", "run", "--locked", "ruff", "check", ".")
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
RECEIPT_NAME = "rehearsal.json"
MUTATION_COUNT_FIELDS = ("killed", "mutants", "score", "survived", "timed_out")
CHECKS = (
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
) -> subprocess.CompletedProcess[str]:
    argv = [str(part) for part in command]
    print(f"+ {shlex.join(argv)}", flush=True)
    return subprocess.run(
        argv,
        check=True,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE if capture else None,
        text=True,
    )


def _sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(HASH_CHUNK_BYTES), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _project_version(root: Path) -> str:
    project = tomllib.loads((root / "pyproject.toml").read_text(encoding="utf-8"))[
        "project"
    ]
    return str(project["version"])


def _playwright_config(root: Path) -> Path:
    expression = (
        "from pathlib import Path; "
        "from spice.agent.driver import write_playwright_mcp_config; "
        "print(write_playwright_mcp_config(Path.cwd()))"
    )
    result = _run(
        ["uv", "run", "--locked", "python", "-c", expression],
        cwd=root,
        capture=True,
    )
    path = Path(result.stdout.strip())
    if not path.is_file():
        raise RehearsalError(f"Playwright config was not materialized: {path}")
    return path


def _run_source_gates(root: Path) -> dict[str, dict[str, object]]:
    _run(PYTHON_GATE_COMMAND, cwd=root)
    _run(RUFF_GATE_COMMAND, cwd=root)
    browser_env = dict(os.environ)  # env-policy: allow
    browser_env[PLAYWRIGHT_CONFIG_ENV] = str(_playwright_config(root))
    _run(BROWSER_GATE_COMMAND, cwd=root, env=browser_env)
    mutation = _run(MUTATION_GATE_COMMAND, cwd=root, capture=True)
    return verify_mutation_cohort(root, mutation.stdout)


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
    root: Path, artifact_dir: Path, version: str
) -> tuple[Path, Path]:
    artifact_dir.mkdir(parents=True, exist_ok=False)
    _run(
        [
            sys.executable,
            "-m",
            "build",
            "--no-isolation",
            "--sdist",
            "--outdir",
            str(artifact_dir),
            str(root),
        ],
        cwd=artifact_dir,
    )
    _run(
        [
            sys.executable,
            "-m",
            "build",
            "--no-isolation",
            "--wheel",
            "--outdir",
            str(artifact_dir),
            str(root),
        ],
        cwd=artifact_dir,
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
        [sys.executable, "-m", "twine", "check", str(sdist), str(wheel)],
        cwd=artifact_dir,
    )
    return sdist, wheel


def _isolated_environment() -> dict[str, str]:
    environment = dict(os.environ)  # env-policy: allow
    for name in ("PYTHONHOME", "PYTHONPATH", "VIRTUAL_ENV"):
        environment.pop(name, None)
    return environment


def _validate_installed_wheel(
    root: Path, wheel: Path, version: str, scratch: Path
) -> None:
    venv = scratch / "installed"
    python = venv / "bin" / "python"
    spice = venv / "bin" / "spice"
    _run([sys.executable, "-m", "venv", str(venv)], cwd=scratch)
    _run(
        ["uv", "pip", "install", "--python", str(python), str(wheel)],
        cwd=scratch,
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
    _run([str(python), "-I", "-c", probe], cwd=scratch, env=environment)
    for arguments in (
        ("--version",),
        ("task", "--help"),
        ("session", "--help"),
    ):
        _run([str(spice), *arguments], cwd=scratch, env=environment)
    if (
        root
        in Path(
            _run(
                [str(python), "-I", "-c", "import spice; print(spice.__path__[0])"],
                cwd=scratch,
                capture=True,
                env=environment,
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


def _rebuild_wheel_from_sdist(sdist: Path, version: str, scratch: Path) -> Path:
    source = _extract_sdist(sdist, scratch / "sdist", version)
    rebuilt_dir = scratch / "rebuilt"
    rebuilt_dir.mkdir()
    _run(
        [
            sys.executable,
            "-m",
            "build",
            "--no-isolation",
            "--wheel",
            "--outdir",
            str(rebuilt_dir),
            str(source),
        ],
        cwd=rebuilt_dir,
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
    version = _project_version(root)
    mutation = _run_source_gates(root)
    with tempfile.TemporaryDirectory(prefix="spice-release-rehearsal-") as raw:
        scratch = Path(raw)
        source = _materialize_committed_source(root, scratch)
        sdist, wheel = _build_canonical_artifacts(source, artifact_dir, version)
        _validate_installed_wheel(root, wheel, version, scratch)
        rebuilt = _rebuild_wheel_from_sdist(sdist, version, scratch)
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
        "checks": list(CHECKS),
        "source_identities": _load_git_private_json(
            root, "release-proof-identities.json"
        ),
        "toolchain": _load_git_private_json(root, "release-proof-toolchain.json"),
        "mutation": mutation,
        "artifacts": {
            "sdist": {"filename": sdist.name, "sha256": sdist_sha256},
            "wheel": {"filename": wheel.name, "sha256": wheel_sha256},
            "installed_wheel_sha256": wheel_sha256,
            "sdist_rebuilt_from_sha256": sdist_sha256,
        },
        "wheel_member_comparison": {
            "canonical_members": len(_wheel_member_hashes(wheel)),
            "rebuilt_members": rebuilt_member_count,
            "mismatches": [],
            "rebuilt_wheel_sha256": rebuilt_sha256,
            "outer_archive_reproducibility": "deferred",
        },
    }
    _write_receipt(artifact_dir / RECEIPT_NAME, receipt)
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--artifacts", required=True, type=Path)
    arguments = parser.parse_args()
    try:
        receipt = rehearse(Path(__file__).resolve().parent.parent, arguments.artifacts)
    except RehearsalError as exc:
        print(f"release-proof rehearsal: {exc}", file=sys.stderr)
        return EXIT_FAILURE
    print(json.dumps(receipt, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
