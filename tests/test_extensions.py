from __future__ import annotations

import pytest

from spice.config import edit, layers, values
from spice.agent.driver import (
    SPICE_AGENT_DRIVER_ENV,
    driver_choices,
    driver_for,
    select_driver,
)
from spice.cli.parser import build_parser
from spice.errors import SpiceError
from spice.extensions import (
    SPICE_DRIVER_ENTRY_POINT_GROUP,
    SPICE_EXTENSION_ENTRY_POINT_GROUPS,
    SPICE_STUDY_ENTRY_POINT_GROUP,
    SPICE_WRAPPER_ENTRY_POINT_GROUP,
    extension_entry_points,
    merge_builtin_and_extension_entry_points,
)
from tests.test_extensionhelpers import build_fixture_distribution, build_fixture_wheel

pytestmark = pytest.mark.usefixtures("git_worktree_tmp_path")


def test_extension_entry_points_query_fixture_wheel_groups_without_importing(
    tmp_path,
):
    _, distribution = build_fixture_distribution(tmp_path)

    discovered = {
        group: {
            entry.name: entry.value
            for entry in extension_entry_points(group, distributions=[distribution])
        }
        for group in SPICE_EXTENSION_ENTRY_POINT_GROUPS
    }

    assert discovered == {
        SPICE_DRIVER_ENTRY_POINT_GROUP: {
            "codex": "test_spiceextensiondriver:SHADOW_CODEX_DRIVER",
            "toy": "test_spiceextensiondriver:TOY_DRIVER",
        },
        SPICE_STUDY_ENTRY_POINT_GROUP: {
            "file-loc": "test_spiceextensionstudy:shadow_file_loc_study",
            "toy-study": "test_spiceextensionstudy:run_toy_study",
        },
        SPICE_WRAPPER_ENTRY_POINT_GROUP: {
            "spice-dev": "test_spiceextensionwrapper:shadow_spice_dev_wrapper_spec",
            "toy-wrapper": "test_spiceextensionwrapper:toy_wrapper_spec",
        },
    }


@pytest.mark.parametrize(
    ("group", "built_in_name"),
    [
        (SPICE_DRIVER_ENTRY_POINT_GROUP, "codex"),
        (SPICE_STUDY_ENTRY_POINT_GROUP, "file-loc"),
        (SPICE_WRAPPER_ENTRY_POINT_GROUP, "spice-dev"),
    ],
)
def test_extension_entry_points_reject_builtin_shadow_names(
    tmp_path, group, built_in_name
):
    _, distribution = build_fixture_distribution(tmp_path)

    with pytest.raises(SpiceError) as exc_info:
        extension_entry_points(
            group,
            built_in_names=[built_in_name],
            distributions=[distribution],
        )

    message = str(exc_info.value)
    assert group in message
    assert built_in_name in message
    assert "shadows built-in" in message


def test_extension_entry_points_reject_duplicate_extension_names_deterministically(
    tmp_path,
):
    _, distribution = build_fixture_distribution(tmp_path)

    with pytest.raises(SpiceError) as exc_info:
        extension_entry_points(
            SPICE_DRIVER_ENTRY_POINT_GROUP,
            distributions=[distribution, distribution],
        )

    assert str(exc_info.value) == (
        "duplicate extension entry point group 'spice.drivers' entry 'codex'; "
        "providers: "
        "spice-extension-fixture:test_spiceextensiondriver:SHADOW_CODEX_DRIVER, "
        "spice-extension-fixture:test_spiceextensiondriver:SHADOW_CODEX_DRIVER"
    )


def test_merge_builtin_and_extension_entry_points_keeps_builtins_first(tmp_path):
    _, distribution = build_fixture_distribution(tmp_path)

    merged = merge_builtin_and_extension_entry_points(
        SPICE_DRIVER_ENTRY_POINT_GROUP,
        {"builtin": object()},
        distributions=[distribution],
    )

    assert tuple(merged) == ("builtin", "codex", "toy")


def test_agent_driver_registry_loads_toy_driver_from_fixture_wheel(
    tmp_path, monkeypatch
):
    wheel = build_fixture_wheel(
        tmp_path,
        entry_points={
            SPICE_DRIVER_ENTRY_POINT_GROUP: {
                "toy": "test_spiceextensiondriver:TOY_DRIVER",
            }
        },
    )
    monkeypatch.syspath_prepend(str(wheel))
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)

    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.AGENT_KEY,
        {values.AGENT_DRIVER_KEY: "toy"},
    )
    parser_args = build_parser().parse_args(["config", "agent", "--driver", "toy"])

    assert driver_choices() == ("claude", "codex", "toy")
    assert select_driver("toy").name == "toy"
    assert driver_for(tmp_path).name == "toy"
    assert parser_args.driver == "toy"
    with pytest.raises(RuntimeError) as exc_info:
        select_driver("missing")
    assert "expected one of: claude, codex, toy" in str(exc_info.value)


def test_agent_driver_registry_rejects_builtin_shadow_from_fixture_wheel(
    tmp_path, monkeypatch
):
    wheel = build_fixture_wheel(tmp_path)
    monkeypatch.syspath_prepend(str(wheel))

    with pytest.raises(SpiceError) as exc_info:
        select_driver("toy")

    message = str(exc_info.value)
    assert SPICE_DRIVER_ENTRY_POINT_GROUP in message
    assert "codex" in message
    assert "shadows built-in" in message
