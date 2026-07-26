"""The durable blocking-surface audit stays synchronized with production."""

from __future__ import annotations

import ast
import re
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
AUDIT_PATH = PROJECT_ROOT / "docs" / "design" / "accepted" / "unbounded-wait-audit.md"
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
    # `Condition.wait_for` blocks exactly as `wait` does. Scanning only `wait`
    # let a signal class that wraps its condition in a named helper carry the
    # blocking call out of view: the audit went on naming the caller, which no
    # longer waits, while the wait itself sat unclassified.
    "wait_for",
}
LOCK_FACTORIES = {"Lock", "RLock", "threading.Lock", "threading.RLock"}
DOC_ANCHOR_RE = re.compile(
    r"(spice/[A-Za-z0-9_./-]+\.py)::([A-Za-z0-9_.<>]+)#([A-Za-z0-9_]+)"
)


def test_blocking_surface_audit_covers_every_deterministically_scanned_call():
    discovered = _production_blocking_call_sites()
    documented = _documented_call_sites(AUDIT_PATH.read_text(encoding="utf-8"))
    missing = sorted(discovered - documented)

    assert documented.issuperset(discovered), (
        "unclassified production blocking surface(s); add each path::function#call "
        f"anchor to {AUDIT_PATH.relative_to(PROJECT_ROOT)} with impact and "
        f"invariant: {missing}"
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


def test_blocking_surface_audit_anchors_name_calls_the_scan_still_finds():
    """Every documented anchor still points at a call this tree makes.

    The coverage test above only asks that each discovered call site be
    documented, so drift the other way is silent: a row can go on naming a
    call that was renamed, moved into a helper, or deleted outright and stay
    green forever, because nothing discovers it to compare against. That is
    how the audit came to carry four anchors for call sites the tree no
    longer had. A surface the scan cannot see is still describable here --
    the rows carry prose for those -- but an anchor is the machine-checked
    form and has to name something real.
    """
    discovered = _production_blocking_call_sites()
    documented = _documented_call_sites(AUDIT_PATH.read_text(encoding="utf-8"))
    stale = sorted(documented - discovered)

    assert discovered.issuperset(documented), (
        "audit anchor(s) name a call this tree no longer makes; re-anchor each "
        "to the surface that exists now, or drop it and leave the description "
        f"as prose, in {AUDIT_PATH.relative_to(PROJECT_ROOT)}: {stale}"
    )


def _production_blocking_call_sites() -> set[str]:
    found: set[str] = set()
    for path in sorted((PROJECT_ROOT / "spice").rglob("*.py")):
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        scopes = _function_scopes(tree)
        rel = path.relative_to(PROJECT_ROOT).as_posix()
        for node in ast.walk(tree):
            if isinstance(node, ast.Call) and _is_blocking_call(node, path):
                qualified = _enclosing_qualified_name(scopes, node.lineno)
                call = _qualified_name(node.func).rsplit(".", 1)[-1]
                found.add(f"{rel}::{qualified}#{call}")
    return found


def _function_scopes(tree: ast.AST) -> list[tuple[int, int, str]]:
    # Each blocking call is anchored to its enclosing function so the audit
    # references survive line drift; the span lets us resolve a call's lineno
    # to the innermost function that contains it.
    scopes: list[tuple[int, int, str]] = []
    stack: list[str] = []

    class _Scopes(ast.NodeVisitor):
        def _enter_function(self, node: ast.FunctionDef | ast.AsyncFunctionDef) -> None:
            stack.append(node.name)
            scopes.append(
                (node.lineno, node.end_lineno or node.lineno, ".".join(stack))
            )
            self.generic_visit(node)
            stack.pop()

        def visit_FunctionDef(self, node: ast.FunctionDef) -> None:
            self._enter_function(node)

        def visit_AsyncFunctionDef(self, node: ast.AsyncFunctionDef) -> None:
            self._enter_function(node)

        def visit_ClassDef(self, node: ast.ClassDef) -> None:
            stack.append(node.name)
            self.generic_visit(node)
            stack.pop()

    _Scopes().visit(tree)
    return scopes


def _enclosing_qualified_name(scopes: list[tuple[int, int, str]], lineno: int) -> str:
    containing = [name for start, end, name in scopes if start <= lineno <= end]
    return containing[-1] if containing else "<module>"


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
    return {
        f"{path}::{qualified}#{call}"
        for path, qualified, call in DOC_ANCHOR_RE.findall(text)
    }
