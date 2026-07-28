"""Serve stays coherent when its installed uv environment is replaced live."""

from __future__ import annotations

import http.client
import json
import os
import shutil
import socket
import subprocess
import sys
import threading
import time
from http import HTTPStatus
from pathlib import Path

import pytest

from spice.errors import SpiceError
from spice.serve import app
from spice.serve import runtimeinstall
from spice.serve.runtimeinstall import RuntimeInstallation

PROJECT_ROOT = Path(__file__).resolve().parents[1]
INSTALL_TIMEOUT_SECONDS = 120
SERVE_TIMEOUT_SECONDS = 15.0


def test_runtime_installation_detects_same_byte_marker_replacement(
    tmp_path: Path,
) -> None:
    executable = tmp_path / "venv" / "bin" / "python"
    entrypoint = executable.with_name("spice")
    metadata = tmp_path / "venv" / "site-packages" / "spice.dist-info" / "METADATA"
    packaged_config = tmp_path / "venv" / "site-packages" / "spice" / "spice.toml"
    for path, content in (
        (executable, "python"),
        (entrypoint, "entrypoint"),
        (metadata, "metadata"),
        (packaged_config, "[serve]"),
    ):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    installation = RuntimeInstallation.capture(
        executable=executable,
        entrypoint=entrypoint,
        distribution_metadata=metadata,
        package_config=packaged_config,
    )
    assert installation.is_current() is True

    replacement = entrypoint.with_suffix(".replacement")
    replacement.write_bytes(entrypoint.read_bytes())
    os.replace(replacement, entrypoint)

    assert installation.is_current() is False


def test_replaced_runtime_request_returns_retryable_response_before_config_load(
    tmp_path: Path,
) -> None:
    markers = tmp_path / "runtime"
    executable = markers / "bin" / "python"
    entrypoint = markers / "bin" / "spice"
    metadata = markers / "site-packages" / "spice.dist-info" / "METADATA"
    packaged_config = markers / "site-packages" / "spice" / "spice.toml"
    for path in (executable, entrypoint, metadata, packaged_config):
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(path.name, encoding="utf-8")
    installation = RuntimeInstallation.capture(
        executable=executable,
        entrypoint=entrypoint,
        distribution_metadata=metadata,
        package_config=packaged_config,
    )
    state = app.ServeState(
        anchor_root=tmp_path,
        runtime_installation=installation,
    )
    server = app._ServeHttpServer(("127.0.0.1", 0), app._ServeHandler, state)
    state.stop_serving = server.shutdown
    state.serve_loop_started.set()
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    host, port = server.server_address[:2]
    packaged_config.unlink()

    try:
        connection = http.client.HTTPConnection(host, port, timeout=2)
        connection.request("GET", "/")
        response = connection.getresponse()
        body = response.read().decode("utf-8")
        connection.close()
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)

    assert response.status == HTTPStatus.SERVICE_UNAVAILABLE
    assert response.getheader("Retry-After") == "1"
    assert body == "spice serve runtime was replaced; restarting\n"
    assert state.runtime_replacement.is_set() is True


def test_runtime_watch_startup_error_uses_named_cleanup_deadline(
    monkeypatch,
) -> None:
    join_timeouts: list[float | None] = []

    class StartupErrorThread:
        def __init__(self, *, target, args, name, daemon):
            del target, name, daemon
            self.args = args

        def start(self) -> None:
            _, _, activated, startup_errors, _ = self.args
            startup_errors.append(RuntimeError("watch registration failed"))
            activated.set()

        def join(self, timeout=None) -> None:
            join_timeouts.append(timeout)

    monkeypatch.setattr(runtimeinstall, "Thread", StartupErrorThread)

    with pytest.raises(SpiceError, match="watch registration failed"):
        runtimeinstall.start_runtime_replacement_watch(
            RuntimeInstallation(markers=()),
            stop_event=threading.Event(),
            on_replacement=lambda: None,
        )

    assert join_timeouts == [runtimeinstall.RUNTIME_WATCH_JOIN_TIMEOUT_SECONDS]


def test_live_serve_reexecs_across_wheel_editable_wheel_replacements(
    tmp_path: Path,
) -> None:
    wheel = _build_wheel(tmp_path)
    environment, tool_bin_directory, tool_environment = _install_test_tool(
        tmp_path, wheel
    )
    probe_root = tmp_path / "probe"
    probe_root.mkdir()
    _run(["git", "init", "-q", "-b", "main"], cwd=probe_root)

    restart_count = _exercise_live_replacements(
        tmp_path=tmp_path,
        wheel=wheel,
        environment=environment,
        tool_bin_directory=tool_bin_directory,
        tool_environment=tool_environment,
        probe_root=probe_root,
    )
    assert restart_count == 2


def _build_wheel(tmp_path: Path) -> Path:
    package_source = tmp_path / "package-source"
    shutil.copytree(
        PROJECT_ROOT,
        package_source,
        ignore=shutil.ignore_patterns(
            ".git",
            ".venv",
            ".spice",
            "build",
            "dist",
            "*.egg-info",
            "__pycache__",
        ),
    )
    wheelhouse = tmp_path / "wheelhouse"
    wheelhouse.mkdir()
    _run(
        ["uv", "build", "--wheel", "--out-dir", str(wheelhouse)],
        cwd=package_source,
    )
    return next(wheelhouse.glob("*.whl"))


def _install_test_tool(
    tmp_path: Path,
    wheel: Path,
) -> tuple[Path, Path, dict[str, str]]:
    tool_directory = tmp_path / "tools"
    tool_bin_directory = tmp_path / "bin"
    tool_environment = {
        **os.environ,  # env-policy: allow
        "UV_TOOL_DIR": str(tool_directory),
        "UV_TOOL_BIN_DIR": str(tool_bin_directory),
    }
    _run(
        [
            "uv",
            "tool",
            "install",
            "--python",
            sys.executable,
            "--force",
            str(wheel),
        ],
        cwd=tmp_path,
        env=tool_environment,
    )
    return (
        tool_directory / "spice-harness",
        tool_bin_directory,
        tool_environment,
    )


def _exercise_live_replacements(
    *,
    tmp_path: Path,
    wheel: Path,
    environment: Path,
    tool_bin_directory: Path,
    tool_environment: dict[str, str],
    probe_root: Path,
) -> int:
    python = _venv_python(environment)
    backend = tmp_path / "backend"
    task_backend = tmp_path / "tasks"
    log = tmp_path / "serve.log"
    port = _available_port()
    environment_vars = {
        **os.environ,  # env-policy: allow
        "PYTHONPATH": "",
        "PYTHONSAFEPATH": "1",
    }
    with log.open("w", encoding="utf-8") as output:
        process = subprocess.Popen(
            [
                str(tool_bin_directory / "spice"),
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                str(port),
                "--backend",
                str(backend),
                "--task-backend",
                str(task_backend),
            ],
            cwd=probe_root,
            env=environment_vars,
            stdout=output,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            _wait_for_http(port)
            _replace_test_tool(
                tmp_path,
                PROJECT_ROOT,
                tool_environment,
                editable=True,
            )
            _wait_for_log(log, "replacement runtime ready; re-executing", count=1)
            _wait_for_http(port)
            editable = _runtime_probe(python, cwd=probe_root)
            assert json.loads(editable["direct_url"])["dir_info"]["editable"] is True
            assert Path(editable["config"]).is_relative_to(PROJECT_ROOT)

            _replace_test_tool(tmp_path, wheel, tool_environment, editable=False)
            _wait_for_log(log, "replacement runtime ready; re-executing", count=2)
            html = _wait_for_http(port)
            wheel_runtime = _runtime_probe(python, cwd=probe_root)
            wheel_direct_url = json.loads(wheel_runtime["direct_url"])
            assert wheel_direct_url.get("dir_info", {}).get("editable") is not True
            assert wheel_direct_url["url"].endswith(".whl")
            assert Path(wheel_runtime["config"]).is_relative_to(environment)
            assert wheel_runtime["config_exists"] == "True"
            assert wheel_runtime["version"] in html
        finally:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
    return log.read_text(encoding="utf-8").count(
        "replacement runtime ready; re-executing"
    )


def _replace_test_tool(
    cwd: Path,
    requirement: Path,
    environment: dict[str, str],
    *,
    editable: bool,
) -> None:
    command = [
        "uv",
        "tool",
        "install",
        "--reinstall",
        "--python",
        sys.executable,
    ]
    if editable:
        command.append("--editable")
    _run([*command, str(requirement)], cwd=cwd, env=environment)


def _runtime_probe(python: Path, *, cwd: Path) -> dict[str, str]:
    probe = """
import importlib.metadata as metadata
from pathlib import Path
from spice import paths
from spice.config.layers import load_packaged_config
from spice.version import runtime_version

distribution = metadata.distribution("spice-harness")
config = paths.runtime_spice_source() / "spice.toml"
load_packaged_config()
print("version=" + runtime_version())
print("config=" + str(config))
print("config_exists=" + str(config.is_file()))
print("direct_url=" + (distribution.read_text("direct_url.json") or ""))
"""
    completed = _run(
        [str(python), "-P", "-c", probe],
        cwd=cwd,
        env={**os.environ, "PYTHONPATH": ""},  # env-policy: allow
    )
    return dict(line.split("=", maxsplit=1) for line in completed.stdout.splitlines())


def _available_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def _wait_for_http(port: int) -> str:
    deadline = time.monotonic() + SERVE_TIMEOUT_SECONDS
    last_error: OSError | None = None
    while time.monotonic() < deadline:
        try:
            connection = http.client.HTTPConnection("127.0.0.1", port, timeout=1)
            connection.request("GET", "/")
            response = connection.getresponse()
            body = response.read().decode("utf-8")
            connection.close()
            if response.status == HTTPStatus.OK:
                return body
        except OSError as exc:
            last_error = exc
        time.sleep(0.05)
    raise AssertionError(f"Serve did not answer successfully: {last_error}")


def _wait_for_log(path: Path, text: str, *, count: int) -> None:
    deadline = time.monotonic() + SERVE_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        if path.read_text(encoding="utf-8").count(text) >= count:
            return
        time.sleep(0.05)
    raise AssertionError(
        f"Serve log never contained {count} occurrences of {text!r}:\n"
        f"{path.read_text(encoding='utf-8')}"
    )


def _venv_python(environment: Path) -> Path:
    executable = "Scripts/python.exe" if sys.platform == "win32" else "bin/python"
    return environment / executable


def _run(
    argv: list[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        argv,
        cwd=cwd,
        env=env,
        check=True,
        capture_output=True,
        text=True,
        timeout=INSTALL_TIMEOUT_SECONDS,
    )
