"""Published RTK companion and rewrite protocol contracts."""

from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parents[1]
RTK_UPSTREAM = "https://github.com/rtk-ai/rtk"
RTK_TRUTH_PATHS = (
    "README.md",
    "CONFIG.md",
    "docs/config/reference.md",
    "docs/cli/wrapper-commands.md",
    "docs/design/INVARIANTS.md",
    "docs/design/accepted/transparent-steering-injection.md",
)


def _read(relative: str) -> str:
    return (PROJECT_ROOT / relative).read_text(encoding="utf-8")


def _collapsed(relative: str) -> str:
    return " ".join(_read(relative).split())


def test_rtk_upstream_is_published_across_truth_surfaces():
    for relative in RTK_TRUTH_PATHS:
        assert RTK_UPSTREAM in _read(relative)

    readme = _read("README.md")
    assert "brew install rtk" in readme
    assert f"cargo install --git {RTK_UPSTREAM}" in readme


def test_rtk_current_rewrite_protocol_is_explicit():
    config = _collapsed("CONFIG.md")
    wrapper_contract = _collapsed("docs/cli/wrapper-commands.md")

    for text in (config, wrapper_contract):
        assert "`0.42.4` or newer" in text
        assert "`rtk rewrite" in text
        assert "Exit `3` with non-empty stdout" in text
        assert "Exit `1` with empty stdout" in text
        assert "Every other exit/stdout combination" in text
        assert "RTK_DB_PATH" in text
        assert ".git/spice/agents/<thread>/rtk/history.db" in text


def test_rtk_command_shape_ownership_matches_the_default_group():
    config = _read("CONFIG.md")
    reference = _read("docs/config/reference.md")
    wrapper_contract = _read("docs/cli/wrapper-commands.md")

    assert "RTK owns command-selection policy" in config
    assert "The built-in `common` group contains" in config
    assert "The built-in `common` group contains one `rtk` wrapper" in wrapper_contract
    assert "RTK rewrite selection happens inside `spice agent run`" in reference
    assert "rg-only grep flags" in wrapper_contract
    assert "native find predicates" in wrapper_contract
    assert "diagnostic git flags" in wrapper_contract
