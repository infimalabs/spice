"""Internal parser seam for C# and JavaScript tree-sitter studies."""

from __future__ import annotations

from dataclasses import dataclass
from functools import lru_cache
from pathlib import Path
from typing import Literal

import tree_sitter_c_sharp as ts_csharp
import tree_sitter_javascript as ts_javascript
from tree_sitter import Language, Node, Parser, Query, Tree

TreeSitterLanguage = Literal["csharp", "javascript"]

_LANGUAGE_BY_SUFFIX: dict[str, TreeSitterLanguage] = {
    ".cjs": "javascript",
    ".cs": "csharp",
    ".js": "javascript",
    ".jsx": "javascript",
    ".mjs": "javascript",
}


@dataclass(frozen=True)
class ParsedTreeSitterSource:
    language: TreeSitterLanguage
    suffix: str
    source: bytes
    tree: Tree

    @property
    def root(self) -> Node:
        return self.tree.root_node


def language_for_suffix(suffix: str) -> TreeSitterLanguage | None:
    normalized = suffix if suffix.startswith(".") else f".{suffix}"
    return _LANGUAGE_BY_SUFFIX.get(normalized.lower())


def language_for_path(path: Path | str) -> TreeSitterLanguage | None:
    return language_for_suffix(Path(path).suffix)


def parse_source(
    path: Path | str, source: str | bytes
) -> ParsedTreeSitterSource | None:
    suffix = Path(path).suffix
    language = language_for_suffix(suffix)
    if language is None:
        return None
    source_bytes = source.encode("utf-8") if isinstance(source, str) else source
    return ParsedTreeSitterSource(
        language=language,
        suffix=suffix.lower(),
        source=source_bytes,
        tree=parser_for_language(language).parse(source_bytes),
    )


def parser_for_suffix(suffix: str) -> Parser | None:
    language = language_for_suffix(suffix)
    if language is None:
        return None
    return parser_for_language(language)


@lru_cache(maxsize=None)
def parser_for_language(language: TreeSitterLanguage) -> Parser:
    return Parser(language_object(language))


def query_for_suffix(suffix: str, source: str) -> Query | None:
    language = language_for_suffix(suffix)
    if language is None:
        return None
    return query_for_language(language, source)


@lru_cache(maxsize=None)
def query_for_language(language: TreeSitterLanguage, source: str) -> Query:
    return Query(language_object(language), source)


@lru_cache(maxsize=None)
def language_object(language: TreeSitterLanguage) -> Language:
    if language == "csharp":
        return Language(ts_csharp.language())
    return Language(ts_javascript.language())
