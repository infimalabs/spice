"""Safe rg wrapper routing contracts."""

from spice.agent.rgwrap import agent_rg_wrapper_command


def test_agent_rg_wrapper_routes_plain_search_to_rtk_grep():
    assert agent_rg_wrapper_command(["needle", "src"]) == [
        "rtk",
        "grep",
        "needle",
        "src",
    ]


def test_agent_rg_wrapper_ignores_argparse_delimiter():
    assert agent_rg_wrapper_command(["--", "-n", "needle", "src"]) == [
        "rtk",
        "grep",
        "needle",
        "src",
        "-n",
    ]


def test_agent_rg_wrapper_reorders_common_rg_flags_after_search_target():
    assert agent_rg_wrapper_command(
        ["-n", "-i", "-S", "-g", "*.py", "needle", "src"]
    ) == [
        "rtk",
        "grep",
        "needle",
        "src",
        "-n",
        "-i",
        "-S",
        "-g",
        "*.py",
    ]


def test_agent_rg_wrapper_routes_long_glob_after_search_target():
    assert agent_rg_wrapper_command(["-n", "--glob", "*.py", "needle", "src"]) == [
        "rtk",
        "grep",
        "needle",
        "src",
        "-n",
        "--glob",
        "*.py",
    ]


def test_agent_rg_wrapper_preserves_extra_paths_after_primary_path():
    assert agent_rg_wrapper_command(["needle", "src", "tests"]) == [
        "rtk",
        "grep",
        "needle",
        "src",
        "tests",
    ]


def test_agent_rg_wrapper_preserves_native_files_mode():
    assert agent_rg_wrapper_command(["--files", "src"]) == ["rg", "--files", "src"]


def test_agent_rg_wrapper_preserves_native_json_mode():
    assert agent_rg_wrapper_command(["--json", "needle", "src"]) == [
        "rg",
        "--json",
        "needle",
        "src",
    ]


def test_agent_rg_wrapper_preserves_native_files_with_matches_mode():
    assert agent_rg_wrapper_command(["-l", "needle", "src"]) == [
        "rg",
        "-l",
        "needle",
        "src",
    ]


def test_agent_rg_wrapper_preserves_native_multi_pattern_mode():
    assert agent_rg_wrapper_command(["-e", "left", "-e", "right", "src"]) == [
        "rg",
        "-e",
        "left",
        "-e",
        "right",
        "src",
    ]
