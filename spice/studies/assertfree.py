"""Assertion-freeness detector: flag test functions with no meaningful assertions.

A test with no assertions (or only trivially-true ones like `assert True`)
constrains no behavior. Such tests pass whether the code is correct or broken —
they are weaker than smoke tests and should be audited.

"Didn't-throw" tests (bare function calls with no assert) are included: they
verify execution but no observable output. Flag for review; some are legitimate
(e.g., checking a side-effect via a mock), but all are candidates.

Library seam: public dataclasses and scan/render helpers are importable by
target-repo tools; underscored names remain private.
"""

from __future__ import annotations

import ast
from dataclasses import dataclass
from pathlib import Path


@dataclass(frozen=True)
class AssertFreeFinding:
    path: str
    function: str
    line: int
    reason: str


def scan_assertfree(
    paths: list[Path],
    *,
    root: Path | None = None,
) -> list[AssertFreeFinding]:
    """Return test functions with no meaningful assertions."""
    findings: list[AssertFreeFinding] = []
    for path in paths:
        if not path.name.startswith("test_") and not path.name.endswith("_test.py"):
            continue
        try:
            tree = ast.parse(path.read_bytes(), filename=str(path))
        except (SyntaxError, OSError):
            continue
        rel = str(path.relative_to(root)) if root else str(path)
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
                if not node.name.startswith("test_"):
                    continue
                finding = _check_function(node, rel)
                if finding:
                    findings.append(finding)
    return findings


def render_assertfree_board(findings: list[AssertFreeFinding]) -> str:
    """Render a text board of assertion-freeness findings."""
    if not findings:
        return "assertion-freeness: no assertion-free tests found"
    lines = [f"assertion-freeness: {len(findings)} assertion-free test(s)"]
    for f in findings:
        lines.append(f"  {f.path}:{f.line}: {f.function}")
        lines.append(f"    {f.reason}")
    return "\n".join(lines)


def _check_function(
    node: ast.FunctionDef | ast.AsyncFunctionDef,
    path: str,
) -> AssertFreeFinding | None:
    assert_nodes = [n for n in ast.walk(node) if isinstance(n, ast.Assert)]
    if not assert_nodes:
        return AssertFreeFinding(
            path=path,
            function=node.name,
            line=node.lineno,
            reason="no assert statements (implicit 'did not raise')",
        )
    trivial = [a for a in assert_nodes if _is_trivial(a)]
    if len(trivial) == len(assert_nodes):
        return AssertFreeFinding(
            path=path,
            function=node.name,
            line=node.lineno,
            reason=f"all {len(assert_nodes)} assertion(s) are trivially true (assert True / assert 1)",
        )
    return None


def _is_trivial(node: ast.Assert) -> bool:
    test = node.test
    if isinstance(test, ast.Constant):
        return bool(test.value)
    if isinstance(test, ast.Compare):
        if (
            len(test.ops) == 1
            and isinstance(test.ops[0], ast.Eq)
            and len(test.comparators) == 1
            and isinstance(test.left, ast.Constant)
            and isinstance(test.comparators[0], ast.Constant)
            and test.left.value == test.comparators[0].value
        ):
            return True
    return False
