"""The product model is portable, navigable, and covered by its case battery."""

from __future__ import annotations

import re
from pathlib import Path
from urllib.parse import unquote, urlsplit

PRODUCT_ROOT = Path(__file__).resolve().parents[1] / "docs" / "product"
MARKDOWN_LINK = re.compile(r"\[[^\]]+\]\(([^)\s]+)\)")
INVARIANT = re.compile(r'<a id="([^"]+)"></a>\*\*([^*]+)\*\*')


def _documents() -> dict[Path, str]:
    return {
        path: path.read_text(encoding="utf-8")
        for path in sorted(PRODUCT_ROOT.rglob("*.md"))
    }


def _local_links(path: Path, text: str) -> set[tuple[Path, str]]:
    links = set()
    for raw in MARKDOWN_LINK.findall(text):
        url = urlsplit(raw)
        if url.scheme or url.netloc:
            continue
        target = (path.parent / unquote(url.path)).resolve() if url.path else path
        links.add((target, unquote(url.fragment)))
    return links


def test_product_model_links_resolve_within_the_portable_tree():
    documents = _documents()
    assert PRODUCT_ROOT / "README.md" in documents
    for path, text in documents.items():
        assert text.startswith("# "), path
        for target, _ in _local_links(path, text):
            assert target.is_relative_to(PRODUCT_ROOT), (path, target)
            assert target in documents, (path, target)


def test_product_model_index_reaches_every_document():
    documents = _documents()
    pending = [PRODUCT_ROOT / "README.md"]
    reachable = set()
    while pending:
        path = pending.pop()
        if path in reachable:
            continue
        reachable.add(path)
        pending.extend(target for target, _ in _local_links(path, documents[path]))
    assert reachable == set(documents)


def test_invariant_names_and_anchors_are_unique():
    registers = sorted((PRODUCT_ROOT / "invariants").glob("*.md"))
    assert registers
    names = []
    for path in registers:
        entries = INVARIANT.findall(path.read_text(encoding="utf-8"))
        assert entries, path
        anchors = [anchor for anchor, _ in entries]
        assert len(anchors) == len(set(anchors)), path
        names.extend(name for _, name in entries)
    assert len(names) == len(set(names))


def test_every_registered_invariant_has_a_case_or_declared_audit():
    documents = _documents()
    registered = {
        (path, anchor)
        for path, text in documents.items()
        if path.parent == PRODUCT_ROOT / "invariants"
        for anchor, _ in INVARIANT.findall(text)
    }
    covered = {
        link
        for path, text in documents.items()
        if path.parent == PRODUCT_ROOT / "cases"
        for link in _local_links(path, text)
    }
    assert registered
    assert covered.issuperset(registered), sorted(registered - covered)
