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


def test_rtk_truth_surfaces_publish_one_configurable_native_fallback_contract():
    shared_contract = (
        "`rtk.executable`",
        "basename or absolute path",
        "Exit `0` or Exit `3` with non-empty stdout",
        "Exit `1` with empty stdout",
        "native command",
        "bounded",
        "diagnostic",
        "canonical `rtk` frontend",
        "built-in `common` wrapper",
        "RTK_DB_PATH",
        ".git/spice/agents/<thread>/rtk/history.db",
        "health telemetry",
    )

    for relative in RTK_TRUTH_PATHS:
        text = _collapsed(relative)
        assert {fragment: fragment in text for fragment in shared_contract} == {
            fragment: True for fragment in shared_contract
        }, relative


def test_rtk_executable_setting_publishes_layering_and_exact_identity_trust():
    config = _collapsed("CONFIG.md")
    reference = _collapsed("docs/config/reference.md")

    assert {
        "config_tracked_table": "[tool.spice.rtk]" in config,
        "config_plain_table": "plain `[rtk]` table" in config,
        "config_four_layers": "standard four-layer precedence" in config,
        "reference_tracked_table": "[tool.spice.rtk]" in reference,
        "reference_plain_table": "use `[rtk]`" in reference,
        "reference_no_lookup": "performs no `which`, existence, or executable probe"
        in reference,
        "reference_exact_identity": "invoke that exact identity" in reference,
    } == {
        "config_tracked_table": True,
        "config_plain_table": True,
        "config_four_layers": True,
        "reference_tracked_table": True,
        "reference_plain_table": True,
        "reference_no_lookup": True,
        "reference_exact_identity": True,
    }


def test_rtk_command_shape_ownership_matches_the_default_group():
    config = _collapsed("CONFIG.md")
    reference = _collapsed("docs/config/reference.md")
    wrapper_contract = _collapsed("docs/cli/wrapper-commands.md")

    assert "RTK owns command-selection policy" in config
    assert "The built-in `common` group contains" in config
    assert "The built-in `common` group contains one `rtk` wrapper" in wrapper_contract
    assert "RTK rewrite selection happens inside `spice agent run`" in reference
    assert "rg-only grep flags" in wrapper_contract
    assert "native find predicates" in wrapper_contract
    assert "diagnostic git flags" in wrapper_contract
