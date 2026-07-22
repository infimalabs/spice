"""Container appliance lifecycle and declared-toolchain contracts."""

from __future__ import annotations

import hashlib
import json
import stat
import subprocess
from pathlib import Path
from typing import cast

import pytest

from tests.test_releaseproofhelpers import (
    APPLIANCE,
    BASE_DIGEST,
    BASE_IMAGE,
    CONTAINERFILE,
    FAKE_COPY_FAILURE_EXIT_CODE,
    PROJECT_ROOT,
    SOURCE_EXPORTER,
    TOOLCHAIN_DECLARATION,
    _file_inventory,
    _test_sha256,
)


class _ApplianceRunner:
    def __init__(
        self,
        engine: str,
        *,
        failure_mode: str | None = None,
    ) -> None:
        self.engine = engine
        self.failure_mode = failure_mode
        self.commit = "a" * 40
        self.tree = "b" * 40
        self.calls: list[tuple[list[str], Path, float]] = []

    def __call__(
        self, command: list[str], cwd: Path, timeout_seconds: float
    ) -> subprocess.CompletedProcess[str]:
        self.calls.append((list(command), cwd, timeout_seconds))
        if command[0] == "git":
            if "status" in command:
                stdout = " M tracked.py\n" if self.failure_mode == "dirty" else ""
            elif command[-1] == "HEAD^{commit}":
                stdout = self.commit + "\n"
            else:
                stdout = self.tree + "\n"
            return subprocess.CompletedProcess(command, 0, stdout=stdout, stderr="")
        if Path(command[0]).name == "release-proof-source":
            context = Path(command[1])
            provenance = context / ".release-proof" / "source.json"
            provenance.parent.mkdir(parents=True)
            provenance.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "source": {
                            "commit": self.commit,
                            "tree": self.tree,
                            "commit_epoch": 1,
                        },
                    }
                ),
                encoding="utf-8",
            )
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
        if command[0] == self.engine:
            verb = command[1]
            if verb == "--version":
                return subprocess.CompletedProcess(
                    command,
                    0,
                    stdout=f"{self.engine} version 99.0\n",
                    stderr="",
                )
            if verb == "build" and self.failure_mode == "deadline":
                raise APPLIANCE.CommandDeadline(
                    command,
                    timeout_seconds,
                    "environment-secret-value\n",
                    _credential_url(),
                )
            if verb == "cp":
                if self.failure_mode == "signal":
                    raise KeyboardInterrupt
                if self.failure_mode == "copy":
                    return subprocess.CompletedProcess(
                        command,
                        FAKE_COPY_FAILURE_EXIT_CODE,
                        stdout="environment-secret-value\n",
                        stderr=_credential_url(),
                    )
                self._write_linux_bundle(
                    Path(command[-1]), corrupt=self.failure_mode == "digest"
                )
            if verb == "container" and self.failure_mode == "cleanup":
                return subprocess.CompletedProcess(
                    command,
                    55,
                    stdout="",
                    stderr="owned container cleanup failed\n",
                )
            return subprocess.CompletedProcess(command, 0, stdout="", stderr="")
        if len(command) > 1 and Path(command[1]).name == "hostnative.py":
            if self.failure_mode == "host-native":
                return subprocess.CompletedProcess(
                    command,
                    61,
                    stdout="",
                    stderr="native companion failed\n",
                )
            evidence_dir = Path(command[-1])
            self._write_macos_companion(evidence_dir)
            return subprocess.CompletedProcess(command, 0, stdout="{}\n", stderr="")
        raise AssertionError(f"unexpected release-proof command: {command}")

    def _write_linux_bundle(self, directory: Path, *, corrupt: bool) -> None:
        wheel = directory / "spice_harness-0.26.0-py3-none-any.whl"
        sdist = directory / "spice_harness-0.26.0.tar.gz"
        wheel.write_bytes(b"tested wheel\n")
        sdist.write_bytes(b"tested sdist\n")
        wheel_digest = _test_sha256(wheel)
        sdist_digest = _test_sha256(sdist)
        report = {
            "schema_version": 1,
            "claim_boundary": {
                "operating_system": "linux",
                "host_native_companion": "release-proof-macos.json",
                "host_native_checks": [
                    "kqueue-or-fsevents",
                    "appearance",
                    "speech",
                ],
            },
            "source_identity": {
                "schema_version": 1,
                "source": {
                    "commit": self.commit,
                    "tree": self.tree,
                    "commit_epoch": 1,
                },
                "synthetic": {"commit": "c" * 40, "tree": "d" * 40},
            },
            "artifacts": {
                "wheel": {
                    "filename": wheel.name,
                    "bytes": wheel.stat().st_size,
                    "sha256": "0" * 64 if corrupt else wheel_digest,
                },
                "sdist": {
                    "filename": sdist.name,
                    "bytes": sdist.stat().st_size,
                    "sha256": sdist_digest,
                },
                "installed_wheel_sha256": wheel_digest,
                "sdist_rebuilt_from_sha256": sdist_digest,
            },
        }
        (directory / "release-proof.json").write_text(
            json.dumps(report, sort_keys=True) + "\n",
            encoding="utf-8",
        )

    def _write_macos_companion(self, directory: Path) -> None:
        linux = (directory / "release-proof.json").read_bytes()
        report = {
            "schema_version": 1,
            "claim_boundary": {
                "operating_system": "macos",
                "container_operating_system": "linux",
                "container_evidence_unchanged": True,
            },
            "container_evidence": {
                "filename": "release-proof.json",
                "sha256": hashlib.sha256(linux).hexdigest(),
            },
            "source_identity": {
                "agreement": "exact",
                "checkout_head": self.commit,
                "container_source_commit": self.commit,
            },
            "checks": {
                "kqueue-or-fsevents": {"status": "passed"},
                "appearance": {"status": "passed"},
                "speech": {"status": "passed"},
            },
        }
        (directory / "release-proof-macos.json").write_text(
            json.dumps(report, sort_keys=True) + "\n",
            encoding="utf-8",
        )


def _credential_url() -> str:
    return (
        "https://user:pass@example.test/callback?access_token=query-secret"
        "#token=fragment-secret\n"
    )


def _appliance_paths(tmp_path: Path) -> tuple[Path, Path]:
    root = tmp_path / "repository"
    (root / "scripts").mkdir(parents=True)
    (root / "release-proof").mkdir()
    return root, tmp_path / "proof-output"


def _run_appliance(
    root: Path,
    output: Path,
    runner: _ApplianceRunner,
    *,
    system_name: str = "Linux",
    run_id: str = "0123456789abcdef",
) -> dict[str, object]:
    return APPLIANCE.run_release_proof(
        root,
        runner.engine,
        output,
        command_runner=runner,
        which=lambda name: f"/fake/{name}",
        system_name=system_name,
        run_id=run_id,
        clock=lambda: "2026-07-21T00:00:00Z",
    )


@pytest.mark.parametrize("engine", ["docker", "podman"])
def test_release_proof_appliance_uses_the_portable_exact_engine_lifecycle(
    tmp_path, engine
):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner(engine)

    result = _run_appliance(root, output, runner)

    engine_commands = [
        command for command, _cwd, _timeout in runner.calls if command[0] == engine
    ]
    build = engine_commands[1]
    context = build[-1]
    image = build[build.index("--tag") + 1]
    container = engine_commands[2][3]
    copy_destination = engine_commands[3][-1]
    assert engine_commands == [
        [engine, "--version"],
        [
            engine,
            "build",
            "--file",
            f"{context}/release-proof/Containerfile",
            "--tag",
            image,
            context,
        ],
        [engine, "create", "--name", container, image, "artifact-carrier"],
        [engine, "cp", f"{container}:/artifacts/.", copy_destination],
        [engine, "container", "rm", container],
        [engine, "image", "rm", image],
    ]
    assert result["status"] == "passed"
    assert result["engine"] == {
        "name": engine,
        "version": f"{engine} version 99.0",
    }
    assert result["source"] == {"commit": runner.commit, "tree": runner.tree}
    assert _file_inventory(output) == [
        "release-proof.json",
        "spice_harness-0.26.0-py3-none-any.whl",
        "spice_harness-0.26.0.tar.gz",
    ]


def test_release_proof_appliance_object_names_are_run_scoped(tmp_path):
    root, first_output = _appliance_paths(tmp_path)
    first = _ApplianceRunner("docker")
    second = _ApplianceRunner("docker")
    second_output = tmp_path / "proof-output-two"

    _run_appliance(root, first_output, first, run_id="1111111111111111")
    _run_appliance(root, second_output, second, run_id="2222222222222222")

    first_create = next(command for command, *_ in first.calls if "create" in command)
    second_create = next(command for command, *_ in second.calls if "create" in command)
    assert (first_create[3], second_create[3]) == (
        "spice-release-proof-aaaaaaaaaaaa-1111111111111111",
        "spice-release-proof-aaaaaaaaaaaa-2222222222222222",
    )


def test_release_proof_appliance_publishes_redacted_failure_and_exact_cleanup(
    tmp_path, monkeypatch
):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("podman", failure_mode="copy")
    monkeypatch.setenv("UV_PUBLISH_TOKEN", "environment-secret-value")

    result = _run_appliance(root, output, runner)
    report = json.loads(
        (output / "release-proof-failure.json").read_text(encoding="utf-8")
    )
    diagnostic_path = output / report["diagnostics"][0]["filename"]
    diagnostic = diagnostic_path.read_text(encoding="utf-8")

    assert result == report
    assert report["status"] == "failed"
    assert report["phase"] == "engine-copy"
    assert report["exit_code"] == FAKE_COPY_FAILURE_EXIT_CODE
    assert report["cleanup"] == {"container": "removed", "image": "removed"}
    assert _file_inventory(output) == [
        "failures/01-engine-copy.log",
        "release-proof-failure.json",
    ]
    assert "<redacted-env:UV_PUBLISH_TOKEN>" in diagnostic
    assert "https://<redacted>@example.test/callback" in diagnostic
    assert "access_token=%3Credacted%3E" in diagnostic
    assert "#token=%3Credacted%3E" in diagnostic
    assert report["diagnostics"] == [
        {
            "filename": "failures/01-engine-copy.log",
            "bytes": diagnostic_path.stat().st_size,
            "sha256": _test_sha256(diagnostic_path),
        }
    ]


@pytest.mark.parametrize(
    ("failure_mode", "phase", "exit_code"),
    [("deadline", "engine-build", 124), ("signal", "signal", 130)],
)
def test_release_proof_appliance_publishes_bounded_interruption_status(
    tmp_path, failure_mode, phase, exit_code
):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker", failure_mode=failure_mode)

    result = _run_appliance(root, output, runner)

    assert result["status"] == "failed"
    assert result["phase"] == phase
    assert result["exit_code"] == exit_code
    assert result["output_published"] is True
    cleanup = cast(dict[str, str], result["cleanup"])
    assert tuple(sorted(cleanup.items())) in (
        (("container", "not-created"), ("image", "not-created")),
        (("container", "removed"), ("image", "removed")),
    )


def test_release_proof_appliance_publishes_digest_validation_evidence(tmp_path):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker", failure_mode="digest")

    result = _run_appliance(root, output, runner)

    assert result["status"] == "failed"
    assert result["phase"] == "artifact-validation"
    assert result["cleanup"] == {"container": "removed", "image": "removed"}
    assert _file_inventory(output) == [
        "failures/01-artifact-validation.log",
        "release-proof-failure.json",
    ]


@pytest.mark.parametrize(
    ("failure_mode", "system_name", "phase", "cleanup"),
    [
        (
            "cleanup",
            "Linux",
            "cleanup-container",
            {"container": "failed", "image": "removed"},
        ),
        (
            "host-native",
            "Darwin",
            "host-native",
            {"container": "removed", "image": "removed"},
        ),
    ],
)
def test_release_proof_appliance_publishes_cleanup_and_host_failure_status(
    tmp_path, failure_mode, system_name, phase, cleanup
):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker", failure_mode=failure_mode)

    result = _run_appliance(root, output, runner, system_name=system_name)

    assert result["status"] == "failed"
    assert result["phase"] == phase
    assert result["cleanup"] == cleanup
    diagnostics = cast(list[dict[str, object]], result["diagnostics"])
    assert diagnostics[0]["filename"] == f"failures/01-{phase}.log"


def test_release_proof_appliance_runs_and_validates_darwin_companion(tmp_path):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker")

    result = _run_appliance(root, output, runner, system_name="Darwin")
    host_command = next(
        command for command, *_ in runner.calls if "hostnative.py" in " ".join(command)
    )
    host = json.loads((output / "release-proof-macos.json").read_text(encoding="utf-8"))

    assert result["status"] == "passed"
    assert result["host_native_companion"] == "release-proof-macos.json"
    assert host_command[1:3] == [
        str(root / "release-proof" / "hostnative.py"),
        "--evidence-dir",
    ]
    assert host["source_identity"] == {
        "agreement": "exact",
        "checkout_head": runner.commit,
        "container_source_commit": runner.commit,
    }
    assert _file_inventory(output) == [
        "release-proof-macos.json",
        "release-proof.json",
        "spice_harness-0.26.0-py3-none-any.whl",
        "spice_harness-0.26.0.tar.gz",
    ]


def test_release_proof_appliance_records_clean_source_preflight_failure(tmp_path):
    root, output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker", failure_mode="dirty")

    result = _run_appliance(root, output, runner)

    assert result["status"] == "failed"
    assert result["phase"] == "source-preflight"
    assert result["cleanup"] == {"container": "not-created", "image": "not-created"}
    assert _file_inventory(output) == [
        "failures/01-source-preflight.log",
        "release-proof-failure.json",
    ]


def test_release_proof_appliance_reports_unsafe_output_without_mutation(tmp_path):
    root, _output = _appliance_paths(tmp_path)
    runner = _ApplianceRunner("docker")

    result = _run_appliance(root, root / "proof-output", runner)

    assert result["status"] == "failed"
    assert result["phase"] == "output-preflight"
    assert result["output_published"] is False
    assert result["diagnostics"] == []
    assert runner.calls == []


def test_container_declares_immutable_base_and_complete_resolved_toolchain():
    declaration = json.loads(TOOLCHAIN_DECLARATION.read_text(encoding="utf-8"))
    containerfile = CONTAINERFILE.read_text(encoding="utf-8")

    assert declaration == {
        "schema_version": 1,
        "base": {
            "image": BASE_IMAGE,
            "digest": BASE_DIGEST,
            "platforms": ["linux/amd64", "linux/arm64"],
        },
        "pinned": {
            "build": "1.3.0",
            "pip": "25.1.1",
            "playwright": "1.61.0",
            "setuptools": "80.9.0",
            "twine": "6.1.0",
            "uv": "0.11.23",
            "wheel": "0.45.1",
        },
    }
    assert f"FROM {BASE_IMAGE}@{BASE_DIGEST}" in containerfile
    assert "COPY --chown=pwuser:pwuser . /proof/source" in containerfile
    assert "RUN python3 release-proof/init-source.py /proof/source" in containerfile
    assert "RUN npm ci" in containerfile
    assert "--output .git/release-proof-toolchain.json" in containerfile
    assert "FROM scratch AS artifact_carrier" in containerfile
    assert "COPY --from=proof /proof/artifacts/ /artifacts/" in containerfile
    assert containerfile.split("FROM scratch AS artifact_carrier\n", 1)[1] == (
        "\nCOPY --from=proof /proof/artifacts/ /artifacts/\n"
    )
    for name, version in (
        ("BUILD", "1.3.0"),
        ("PIP", "25.1.1"),
        ("SETUPTOOLS", "80.9.0"),
        ("TWINE", "6.1.0"),
        ("UV", "0.11.23"),
        ("WHEEL", "0.45.1"),
    ):
        assert f"ARG {name}_VERSION={version}" in containerfile
    assert SOURCE_EXPORTER.stat().st_mode & stat.S_IXUSR == stat.S_IXUSR
    assert (PROJECT_ROOT / "scripts" / "release-proof").stat().st_mode & stat.S_IXUSR
