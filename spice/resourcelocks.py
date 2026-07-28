"""`spice lock`: hold configured resource locks around child commands."""

from __future__ import annotations

import argparse
import json
import os
from collections.abc import Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

from spice import defaults
from spice.errors import SpiceError
from spice.locking import FileLockUnavailable, lock_fd_exclusive, unlock_fd
from spice.paths import require_repo_root
from spice.process.tool import run_parent_lifetime_command
from spice.config.layers import effective_context, effective_table

DEFAULT_LOCK_CONTENTION_EXIT_CODE = defaults.integer(
    "locks", "lock_contention_exit_code"
)
DEFAULT_CHOSEN_SHARD_CONTENTION_EXIT_CODE = defaults.integer(
    "locks", "chosen_shard_contention_exit_code"
)
DEFAULT_POOL_EXHAUSTION_EXIT_CODE = defaults.integer(
    "locks", "pool_exhaustion_exit_code"
)
MAX_EXIT_CODE = 255
LOCK_STATE_ROOT = Path(defaults.string("locks", "state_root"))


@dataclass(frozen=True)
class LockExitCodes:
    lock_contention: int
    chosen_shard_contention: int
    pool_exhaustion: int


@dataclass(frozen=True)
class NamedLockConfig:
    name: str
    path: Path
    contention_exit_code: int


@dataclass(frozen=True)
class LockPoolConfig:
    name: str
    directory: Path
    shards: int
    chosen_shard_contention_exit_code: int
    pool_exhaustion_exit_code: int


@dataclass(frozen=True)
class LockSettings:
    locks: dict[str, NamedLockConfig]
    pools: dict[str, LockPoolConfig]


def configure_lock_parser(subparsers: Any) -> None:
    parser = subparsers.add_parser(
        "lock",
        help="Hold configured resource locks while running commands.",
        description="Hold configured resource locks while running commands.",
    )
    actions = parser.add_subparsers(dest="lock_action", required=True)

    run = actions.add_parser(
        "run",
        help="Run a child command while holding a named lock or pool shard.",
    )
    run.add_argument("name", help="Configured lock or pool name.")
    mode = run.add_mutually_exclusive_group()
    mode.add_argument(
        "--lock",
        dest="resource_kind",
        action="store_const",
        const="lock",
        help="Treat NAME as a single configured lock.",
    )
    mode.add_argument(
        "--pool",
        dest="resource_kind",
        action="store_const",
        const="pool",
        help="Treat NAME as a configured shard pool.",
    )
    run.add_argument("--shard", type=int, help="Required zero-based pool shard.")
    run.add_argument("--path", help="Override the configured single-lock path.")
    run.add_argument("--directory", help="Override the configured pool directory.")
    run.add_argument("--shards", type=int, help="Override the configured shard count.")
    run.add_argument(
        "--lock-contention-exit-code",
        type=int,
        help="Exit code when a single lock is already held.",
    )
    run.add_argument(
        "--chosen-shard-contention-exit-code",
        type=int,
        help="Exit code when an explicitly chosen pool shard is already held.",
    )
    run.add_argument(
        "--pool-exhaustion-exit-code",
        type=int,
        help="Exit code when every pool shard is already held.",
    )
    run.add_argument(
        "child",
        nargs="+",
        metavar="COMMAND",
        help="Child command argv to run while the lock is held.",
    )
    run.set_defaults(func=handle_lock)

    status = actions.add_parser(
        "status",
        help="List configured locks and pool shards with holder metadata.",
    )
    status.add_argument(
        "--json",
        action="store_true",
        help="Print the status listing as JSON.",
    )
    status.set_defaults(func=handle_lock)


def handle_lock(args: argparse.Namespace) -> int:
    repo_root = require_repo_root()
    if args.lock_action == "run":
        return _handle_run(args, repo_root)
    if args.lock_action == "status":
        return _handle_status(args, repo_root)
    raise SpiceError(f"unknown lock action {args.lock_action!r}")


def configured_lock_settings(repo_root: Path) -> LockSettings:
    table = effective_table(repo_root, "locks")
    defaults = LockExitCodes(
        lock_contention=_exit_code(
            table.get("lock_contention_exit_code"),
            DEFAULT_LOCK_CONTENTION_EXIT_CODE,
            effective_context(repo_root, "locks", "lock_contention_exit_code"),
        ),
        chosen_shard_contention=_exit_code(
            table.get("chosen_shard_contention_exit_code"),
            DEFAULT_CHOSEN_SHARD_CONTENTION_EXIT_CODE,
            effective_context(repo_root, "locks", "chosen_shard_contention_exit_code"),
        ),
        pool_exhaustion=_exit_code(
            table.get("pool_exhaustion_exit_code"),
            DEFAULT_POOL_EXHAUSTION_EXIT_CODE,
            effective_context(repo_root, "locks", "pool_exhaustion_exit_code"),
        ),
    )
    return LockSettings(
        locks=_configured_named_locks(repo_root, table.get("named"), defaults),
        pools=_configured_lock_pools(repo_root, table.get("pools"), defaults),
    )


def _handle_run(args: argparse.Namespace, repo_root: Path) -> int:
    child = _child_argv(args.child)
    settings = configured_lock_settings(repo_root)
    if _selects_pool(args, settings):
        pool = _pool_config_from_args(args, repo_root, settings)
        return _run_pool(pool, args.shard, child)
    lock = _lock_config_from_args(args, repo_root, settings)
    return _run_named_lock(lock, child)


def _handle_status(args: argparse.Namespace, repo_root: Path) -> int:
    records = lock_status_records(configured_lock_settings(repo_root))
    if args.json:
        print(json.dumps(records, indent=2, sort_keys=True))
    else:
        print(_render_status_text(records), end="")
    return 0


def lock_status_records(settings: LockSettings) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for lock in sorted(settings.locks.values(), key=lambda item: item.name):
        state, holder = _lock_state(lock.path)
        records.append(
            {
                "kind": "lock",
                "name": lock.name,
                "path": str(lock.path),
                "state": state,
                "holder": holder,
            }
        )
    for pool in sorted(settings.pools.values(), key=lambda item: item.name):
        for shard in range(pool.shards):
            path = _pool_shard_path(pool, shard)
            state, holder = _lock_state(path)
            records.append(
                {
                    "kind": "pool",
                    "name": pool.name,
                    "path": str(path),
                    "shard": shard,
                    "shards": pool.shards,
                    "state": state,
                    "holder": holder,
                }
            )
    return records


def _configured_named_locks(
    repo_root: Path, raw: object, defaults: LockExitCodes
) -> dict[str, NamedLockConfig]:
    if raw is None:
        return {}
    if not isinstance(raw, dict):
        raise SpiceError("[locks.named] must be a table")
    locks: dict[str, NamedLockConfig] = {}
    for name, value in raw.items():
        if not isinstance(value, dict):
            raise SpiceError(f"[locks.named.{name}] must be a table")
        lock_name = str(name)
        locks[lock_name] = NamedLockConfig(
            name=lock_name,
            path=_config_path(
                repo_root,
                value.get("path"),
                LOCK_STATE_ROOT / f"{lock_name}.lock",
                f"[locks.named.{lock_name}].path",
            ),
            contention_exit_code=_exit_code(
                value.get("contention_exit_code"),
                defaults.lock_contention,
                f"[locks.named.{lock_name}].contention_exit_code",
            ),
        )
    return locks


def _configured_lock_pools(
    repo_root: Path, raw: object, defaults: LockExitCodes
) -> dict[str, LockPoolConfig]:
    if raw is None:
        return {}
    if not isinstance(raw, dict):
        raise SpiceError("[locks.pools] must be a table")
    pools: dict[str, LockPoolConfig] = {}
    for name, value in raw.items():
        if not isinstance(value, dict):
            raise SpiceError(f"[locks.pools.{name}] must be a table")
        pool_name = str(name)
        pools[pool_name] = LockPoolConfig(
            name=pool_name,
            directory=_config_path(
                repo_root,
                value.get("directory"),
                LOCK_STATE_ROOT / pool_name,
                f"[locks.pools.{pool_name}].directory",
            ),
            shards=_positive_int(
                value.get("shards"),
                1,
                f"[locks.pools.{pool_name}].shards",
            ),
            chosen_shard_contention_exit_code=_exit_code(
                value.get("chosen_shard_contention_exit_code"),
                defaults.chosen_shard_contention,
                f"[locks.pools.{pool_name}].chosen_shard_contention_exit_code",
            ),
            pool_exhaustion_exit_code=_exit_code(
                value.get("pool_exhaustion_exit_code"),
                defaults.pool_exhaustion,
                f"[locks.pools.{pool_name}].pool_exhaustion_exit_code",
            ),
        )
    return pools


def _selects_pool(args: argparse.Namespace, settings: LockSettings) -> bool:
    if args.resource_kind == "pool":
        return True
    if args.resource_kind == "lock":
        return False
    poolish = (
        args.shard is not None
        or args.directory
        or args.shards is not None
        or args.chosen_shard_contention_exit_code is not None
        or args.pool_exhaustion_exit_code is not None
    )
    lockish = args.path or args.lock_contention_exit_code is not None
    if poolish and lockish:
        raise SpiceError("lock run received both lock and pool override flags")
    if poolish:
        return True
    if lockish:
        return False
    in_locks = args.name in settings.locks
    in_pools = args.name in settings.pools
    if in_locks and in_pools:
        raise SpiceError(f"resource {args.name!r} exists as both lock and pool")
    if in_pools:
        return True
    return False


def _lock_config_from_args(
    args: argparse.Namespace, repo_root: Path, settings: LockSettings
) -> NamedLockConfig:
    lock = settings.locks.get(args.name)
    if lock is None:
        raise SpiceError(f"unknown configured lock {args.name!r}")
    if args.directory or args.shards is not None or args.shard is not None:
        raise SpiceError("single locks do not accept pool shard flags")
    path = _override_path(repo_root, args.path) or lock.path
    exit_code = _exit_code(
        args.lock_contention_exit_code,
        lock.contention_exit_code,
        "--lock-contention-exit-code",
    )
    return NamedLockConfig(
        name=lock.name,
        path=path,
        contention_exit_code=exit_code,
    )


def _pool_config_from_args(
    args: argparse.Namespace, repo_root: Path, settings: LockSettings
) -> LockPoolConfig:
    pool = settings.pools.get(args.name)
    if pool is None:
        raise SpiceError(f"unknown configured lock pool {args.name!r}")
    if args.path or args.lock_contention_exit_code is not None:
        raise SpiceError("lock pools do not accept single-lock override flags")
    directory = _override_path(repo_root, args.directory) or pool.directory
    shards = _positive_int(args.shards, pool.shards, "--shards")
    chosen_code = _exit_code(
        args.chosen_shard_contention_exit_code,
        pool.chosen_shard_contention_exit_code,
        "--chosen-shard-contention-exit-code",
    )
    exhaustion_code = _exit_code(
        args.pool_exhaustion_exit_code,
        pool.pool_exhaustion_exit_code,
        "--pool-exhaustion-exit-code",
    )
    return LockPoolConfig(
        name=pool.name,
        directory=directory,
        shards=shards,
        chosen_shard_contention_exit_code=chosen_code,
        pool_exhaustion_exit_code=exhaustion_code,
    )


def _run_named_lock(lock: NamedLockConfig, child: list[str]) -> int:
    metadata = _holder_metadata("lock", lock.name, lock.path)
    try:
        with _metadata_lock(lock.path, metadata):
            return _run_child(child)
    except FileLockUnavailable:
        print(f"spice lock: lock {lock.name!r} is already held")
        return lock.contention_exit_code


def _run_pool(pool: LockPoolConfig, shard: int | None, child: list[str]) -> int:
    if shard is not None:
        _validate_shard(pool, shard)
        path = _pool_shard_path(pool, shard)
        metadata = _holder_metadata("pool", pool.name, path, shard=shard)
        try:
            with _metadata_lock(path, metadata):
                return _run_child(child)
        except FileLockUnavailable:
            print(f"spice lock: pool {pool.name!r} shard {shard} is already held")
            return pool.chosen_shard_contention_exit_code
    for candidate in range(pool.shards):
        path = _pool_shard_path(pool, candidate)
        metadata = _holder_metadata("pool", pool.name, path, shard=candidate)
        try:
            with _metadata_lock(path, metadata):
                return _run_child(child)
        except FileLockUnavailable:
            continue
    print(f"spice lock: pool {pool.name!r} has no free shards")
    return pool.pool_exhaustion_exit_code


@contextmanager
def _metadata_lock(path: Path, metadata: dict[str, Any]) -> Iterator[None]:
    path.parent.mkdir(parents=True, exist_ok=True)
    handle = path.open("a+")
    try:
        lock_fd_exclusive(handle.fileno(), blocking=False)
    except BaseException:
        handle.close()
        raise
    try:
        _write_metadata(handle, metadata)
        yield
    finally:
        try:
            _write_metadata(handle, {})
        finally:
            unlock_fd(handle.fileno())
            handle.close()


def _write_metadata(handle: Any, metadata: dict[str, Any]) -> None:
    handle.seek(0)
    handle.truncate()
    if metadata:
        handle.write(json.dumps(metadata, sort_keys=True) + "\n")
    handle.flush()
    os.fsync(handle.fileno())


def _lock_state(path: Path) -> tuple[str, dict[str, Any] | None]:
    if not path.exists():
        return "free", None
    handle = path.open("a+")
    try:
        try:
            lock_fd_exclusive(handle.fileno(), blocking=False)
        except FileLockUnavailable:
            handle.seek(0)
            holder = _metadata_from_text(handle.read())
            return "held", holder
        unlock_fd(handle.fileno())
        return "free", None
    finally:
        handle.close()


def _metadata_from_text(text: str) -> dict[str, Any] | None:
    try:
        loaded = json.loads(text)
    except json.JSONDecodeError:
        return None
    return loaded if isinstance(loaded, dict) and loaded else None


def _render_status_text(records: list[dict[str, Any]]) -> str:
    if not records:
        return "locks: none\n"
    lines: list[str] = []
    for record in records:
        if record["kind"] == "pool":
            line = (
                f"pool {record['name']} shard={record['shard']}/{record['shards']} "
                f"state={record['state']} path={record['path']}"
            )
        else:
            line = (
                f"lock {record['name']} state={record['state']} path={record['path']}"
            )
        holder = record.get("holder")
        if isinstance(holder, dict):
            line += (
                f" pid={holder.get('pid', '-')}"
                f" cwd={holder.get('cwd', '-')}"
                f" started_at={holder.get('started_at', '-')}"
            )
        lines.append(line)
    return "\n".join(lines) + "\n"


def _holder_metadata(
    kind: str, name: str, path: Path, *, shard: int | None = None
) -> dict[str, Any]:
    metadata: dict[str, Any] = {
        "pid": os.getpid(),
        "cwd": str(Path.cwd()),
        "started_at": datetime.now(UTC).isoformat().replace("+00:00", "Z"),
        "kind": kind,
        "name": name,
        "path": str(path),
    }
    if shard is not None:
        metadata["shard"] = shard
    return metadata


def _child_argv(raw: list[str]) -> list[str]:
    child = list(raw or [])
    if child[:1] == ["--"]:
        child = child[1:]
    if not child:
        raise SpiceError("lock run requires a child command after --")
    return child


def _run_child(child: list[str]) -> int:
    try:
        result = run_parent_lifetime_command(child, check=False)
    except FileNotFoundError as exc:
        raise SpiceError(f"child command not found: {child[0]}") from exc
    except OSError as exc:
        raise SpiceError(f"child command failed to start: {exc}") from exc
    return int(result.returncode)


def _pool_shard_path(pool: LockPoolConfig, shard: int) -> Path:
    return pool.directory / f"{shard}.lock"


def _validate_shard(pool: LockPoolConfig, shard: int) -> None:
    if shard < 0 or shard >= pool.shards:
        raise SpiceError(
            f"pool {pool.name!r} shard must be between 0 and {pool.shards - 1}"
        )


def _config_path(repo_root: Path, raw: object, default: Path, label: str) -> Path:
    if raw is None:
        return (repo_root / default).resolve()
    if not isinstance(raw, str) or not raw.strip():
        raise SpiceError(f"{label} must be a non-empty string")
    return _resolve_path(repo_root, raw.strip())


def _override_path(repo_root: Path, raw: str | None) -> Path | None:
    if raw is None:
        return None
    if not raw.strip():
        raise SpiceError("path override must be non-empty")
    return _resolve_path(repo_root, raw.strip())


def _resolve_path(repo_root: Path, raw: str) -> Path:
    path = Path(raw).expanduser()
    return path.resolve() if path.is_absolute() else (repo_root / path).resolve()


def _positive_int(raw: object, default: int, label: str) -> int:
    if raw is None:
        return default
    if isinstance(raw, int):
        value = raw
    elif isinstance(raw, str):
        try:
            value = int(raw)
        except ValueError as exc:
            raise SpiceError(f"{label} must be a positive integer") from exc
    else:
        raise SpiceError(f"{label} must be a positive integer")
    if value <= 0:
        raise SpiceError(f"{label} must be a positive integer")
    return value


def _exit_code(raw: object, default: int, label: str) -> int:
    if raw is None:
        return default
    if isinstance(raw, int):
        value = raw
    elif isinstance(raw, str):
        try:
            value = int(raw)
        except ValueError as exc:
            raise SpiceError(
                f"{label} must be an exit code from 1 to {MAX_EXIT_CODE}"
            ) from exc
    else:
        raise SpiceError(f"{label} must be an exit code from 1 to {MAX_EXIT_CODE}")
    if value < 1 or value > MAX_EXIT_CODE:
        raise SpiceError(f"{label} must be an exit code from 1 to {MAX_EXIT_CODE}")
    return value
