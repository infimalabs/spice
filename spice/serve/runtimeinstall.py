"""Detect and cross live replacements of the running Spice installation."""

from __future__ import annotations

import os
import stat
import subprocess
import sys
import time
from dataclasses import dataclass
from importlib import metadata
from pathlib import Path
from threading import Event, Thread
from typing import Callable

from spice import paths
from spice.errors import SpiceError
from spice.serve.livebuswatch import FileChangeWatch

RUNTIME_WATCH_ACTIVATION_TIMEOUT_SECONDS = 5.0
RUNTIME_WATCH_JOIN_TIMEOUT_SECONDS = 1.0
RUNTIME_RESTART_WAIT_SECONDS = 15.0
RUNTIME_RESTART_RETRY_SECONDS = 0.05
RUNTIME_RESTART_PROBE_TIMEOUT_SECONDS = 2.0
_RUNTIME_RESTART_PROBE = (
    "from spice.config.layers import load_packaged_config; load_packaged_config()"
)

_FileRevision = tuple[int, int, int, int, int, int, str | None] | None


@dataclass(frozen=True, slots=True)
class _RuntimeMarker:
    path: Path
    revision: _FileRevision


@dataclass(frozen=True, slots=True)
class RuntimeInstallation:
    """The file identities that make one running environment coherent."""

    markers: tuple[_RuntimeMarker, ...]

    @classmethod
    def capture(
        cls,
        *,
        executable: Path | None = None,
        entrypoint: Path | None = None,
        distribution_metadata: Path | None = None,
        package_config: Path | None = None,
    ) -> RuntimeInstallation:
        executable = executable or Path(sys.executable)
        entrypoint = entrypoint or executable.with_name(
            "spice.exe" if os.name == "nt" else "spice"
        )
        if distribution_metadata is None:
            distribution_metadata = _distribution_metadata_path()
        package_config = package_config or (paths.runtime_spice_source() / "spice.toml")
        marker_paths = tuple(
            dict.fromkeys(
                path
                for path in (
                    executable,
                    entrypoint,
                    distribution_metadata,
                    package_config,
                )
                if path is not None
            )
        )
        return cls(
            markers=tuple(
                _RuntimeMarker(path=path, revision=_file_revision(path))
                for path in marker_paths
            )
        )

    def is_current(self) -> bool:
        """Whether every installed file still has its startup identity."""
        return all(
            _file_revision(marker.path) == marker.revision for marker in self.markers
        )

    def watch_paths(self) -> tuple[Path, ...]:
        """Existing files and durable ancestors that expose replacement events."""
        candidates: list[Path] = []
        for marker in self.markers:
            candidates.extend(
                (marker.path, marker.path.parent, marker.path.parent.parent)
            )
        return tuple(dict.fromkeys(path for path in candidates if path.exists()))


def start_runtime_replacement_watch(
    installation: RuntimeInstallation,
    *,
    stop_event: Event,
    on_replacement: Callable[[], None],
) -> Thread:
    """Arm a native watcher before Serve accepts requests."""
    activated = Event()
    startup_errors: list[Exception] = []
    thread = Thread(
        target=_run_runtime_replacement_watch,
        args=(
            installation,
            stop_event,
            activated,
            startup_errors,
            on_replacement,
        ),
        name="spice-serve-runtime-watch",
        daemon=True,
    )
    thread.start()
    if not activated.wait(timeout=RUNTIME_WATCH_ACTIVATION_TIMEOUT_SECONDS):
        stop_event.set()
        raise SpiceError(
            "spice serve runtime watcher activation deadline exceeded "
            f"budget={RUNTIME_WATCH_ACTIVATION_TIMEOUT_SECONDS:g}s"
        )
    if startup_errors:
        thread.join(timeout=RUNTIME_WATCH_JOIN_TIMEOUT_SECONDS)
        error = startup_errors[0]
        raise SpiceError(f"spice serve runtime watch failed: {error}") from error
    if not installation.is_current():
        on_replacement()
    return thread


def restart_replaced_runtime(argv: list[str]) -> None:
    """Wait for the replacement to import cleanly, then enter it in place."""
    executable = Path(sys.executable)
    deadline = time.monotonic() + RUNTIME_RESTART_WAIT_SECONDS
    last_error = "replacement executable is unavailable"
    while time.monotonic() < deadline:
        try:
            completed = subprocess.run(
                [
                    str(executable),
                    "-P",
                    "-c",
                    _RUNTIME_RESTART_PROBE,
                ],
                check=False,
                capture_output=True,
                text=True,
                timeout=RUNTIME_RESTART_PROBE_TIMEOUT_SECONDS,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            last_error = str(exc)
        else:
            if completed.returncode == 0:
                print(
                    "spice serve: replacement runtime ready; re-executing",
                    flush=True,
                )
                os.execv(
                    str(executable),
                    [str(executable), "-m", "spice", *argv],
                )
                raise RuntimeError("os.execv returned after replacing Serve")
            last_error = (completed.stderr or completed.stdout).strip()
        time.sleep(RUNTIME_RESTART_RETRY_SECONDS)
    raise SpiceError(
        "spice serve replacement runtime did not become ready within "
        f"{RUNTIME_RESTART_WAIT_SECONDS:g}s: {last_error or 'probe failed'}"
    )


def _run_runtime_replacement_watch(
    installation: RuntimeInstallation,
    stop_event: Event,
    activated: Event,
    startup_errors: list[Exception],
    on_replacement: Callable[[], None],
) -> None:
    watch = FileChangeWatch()
    try:
        while watch.wait(
            installation.watch_paths(),
            stop_event,
            activated=activated,
        ):
            if installation.is_current():
                continue
            on_replacement()
            return
    except Exception as exc:
        if activated.is_set():
            print(f"spice serve: runtime watcher failed: {exc}", flush=True)
        else:
            startup_errors.append(exc)
            activated.set()
    finally:
        watch.close()


def _distribution_metadata_path() -> Path | None:
    try:
        distribution = metadata.distribution("spice-harness")
    except metadata.PackageNotFoundError:
        return None
    path = getattr(distribution, "_path", None)
    return Path(path) / "METADATA" if path is not None else None


def _file_revision(path: Path) -> _FileRevision:
    try:
        observed = path.lstat()
    except OSError:
        return None
    link_target = os.readlink(path) if stat.S_ISLNK(observed.st_mode) else None
    return (
        observed.st_dev,
        observed.st_ino,
        observed.st_mode,
        observed.st_size,
        observed.st_mtime_ns,
        observed.st_ctime_ns,
        link_target,
    )
