"""The durable blocking-surface audit stays synchronized with production."""

from __future__ import annotations

import ast
import re
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
AUDIT_PATH = (
    PROJECT_ROOT / "docs" / "design" / "experimental" / "unbounded-wait-audit.md"
)
DIRECT_BLOCKING_CALLS = {
    "fcntl.flock",
    "select.select",
    "sqlite3.connect",
    "subprocess.Popen",
    "subprocess.run",
    "urllib.request.urlopen",
}
BLOCKING_METHODS = {
    "accept",
    "connect",
    "recv",
    "result",
    "sendall",
    "serve_forever",
    "wait",
}
LOCK_FACTORIES = {"Lock", "RLock", "threading.Lock", "threading.RLock"}
DOC_REFERENCE_RE = re.compile(r"(spice/[A-Za-z0-9_./-]+\.py):([0-9,-]+)")


def test_blocking_surface_audit_covers_every_deterministically_scanned_call():
    discovered = _production_blocking_call_sites()
    documented = _documented_call_sites(AUDIT_PATH.read_text(encoding="utf-8"))
    missing = sorted(discovered - documented)

    assert documented.issuperset(discovered), (
        "unclassified production blocking surface(s); add each file:line to "
        f"{AUDIT_PATH.relative_to(PROJECT_ROOT)} with impact and invariant: {missing}"
    )


def test_blocking_surface_audit_rows_name_classification_and_actionable_owner():
    rows = [
        line
        for line in AUDIT_PATH.read_text(encoding="utf-8").splitlines()
        if line.startswith("| `spice/")
    ]
    classification_labels = ("Bounded", "Lifetime", "Actionable", "actionable")

    assert rows
    assert all(any(label in row for label in classification_labels) for row in rows)
    actionable_rows = [row for row in rows if "actionable" in row.lower()]
    assert all(
        re.search(r"`[A-Z][A-Z0-9]*-1k[A-Za-z0-9]+`", row) for row in actionable_rows
    )


def _production_blocking_call_sites() -> set[str]:
    found: set[str] = set()
    for path in sorted((PROJECT_ROOT / "spice").rglob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        for node in ast.walk(tree):
            if isinstance(node, ast.Call) and _is_blocking_call(node, path):
                found.add(f"{path.relative_to(PROJECT_ROOT).as_posix()}:{node.lineno}")
    return found


def _is_blocking_call(node: ast.Call, path: Path) -> bool:
    qualified = _qualified_name(node.func)
    if qualified == "fcntl.flock":
        return _is_lock_acquisition(node)
    if qualified in DIRECT_BLOCKING_CALLS or qualified in LOCK_FACTORIES:
        return True
    if qualified == "field" and _field_constructs_lock(node):
        return True
    method = qualified.rsplit(".", 1)[-1]
    if method in BLOCKING_METHODS:
        if method == "connect":
            return _receiver_name_contains(node.func, ("socket", "connection"))
        if method == "accept":
            return _receiver_name_contains(
                node.func, ("listener", "socket", "server", "connection")
            )
        return True
    if method == "join":
        return _receiver_name_contains(node.func, ("thread", "worker", "watch"))
    if method == "get":
        return _receiver_name_endswith(node.func, "_queue")
    if method == "read":
        receiver = (
            _qualified_name(node.func.value)
            if isinstance(node.func, ast.Attribute)
            else ""
        )
        return receiver in {"sys.stdin", "stream"} or path.relative_to(
            PROJECT_ROOT
        ).as_posix() in {
            "spice/serve/app.py",
            "spice/serve/websocket.py",
        }
    return False


def _field_constructs_lock(node: ast.Call) -> bool:
    return any(
        keyword.arg == "default_factory"
        and _qualified_name(keyword.value) in LOCK_FACTORIES
        for keyword in node.keywords
    )


def _is_lock_acquisition(node: ast.Call) -> bool:
    if len(node.args) < 2:
        return True
    return _qualified_name(node.args[1]) != "fcntl.LOCK_UN"


def _receiver_name_contains(func: ast.expr, needles: tuple[str, ...]) -> bool:
    if not isinstance(func, ast.Attribute):
        return False
    receiver = _qualified_name(func.value).lower()
    return any(needle in receiver for needle in needles)


def _receiver_name_endswith(func: ast.expr, suffix: str) -> bool:
    if not isinstance(func, ast.Attribute):
        return False
    return _qualified_name(func.value).lower().endswith(suffix)


def _qualified_name(node: ast.expr) -> str:
    if isinstance(node, ast.Name):
        return node.id
    if isinstance(node, ast.Attribute):
        prefix = _qualified_name(node.value)
        return f"{prefix}.{node.attr}" if prefix else node.attr
    return ""


def _documented_call_sites(text: str) -> set[str]:
    found: set[str] = set()
    for match in DOC_REFERENCE_RE.finditer(text):
        path, specification = match.groups()
        for portion in specification.split(","):
            if "-" in portion:
                start, end = (int(value) for value in portion.split("-", 1))
                found.update(f"{path}:{line}" for line in range(start, end + 1))
            else:
                found.add(f"{path}:{int(portion)}")
    return found
