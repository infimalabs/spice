"""Top-level CLI version reporting."""

from __future__ import annotations

from importlib import metadata

import pytest

from spice.cli.parser import build_parser
from spice.version import runtime_version


def test_runtime_version_matches_installed_distribution():
    assert runtime_version() == metadata.version("spice-harness")


@pytest.mark.parametrize("flag", ["--version", "-V"])
def test_root_version_flag_reports_installed_distribution(flag, capsys):
    with pytest.raises(SystemExit) as exc_info:
        build_parser().parse_args([flag])

    assert exc_info.value.code == 0
    assert capsys.readouterr().out == f"spice {runtime_version()}\n"


def test_root_help_documents_version_flags():
    assert "-V, --version" in build_parser().format_help()
