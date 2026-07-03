from __future__ import annotations

import csv
import io
import tomllib
import zipfile
from importlib import metadata
from pathlib import Path

import pytest

from spice import config
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

PROJECT_ROOT = Path(__file__).resolve().parent.parent
FIXTURE_ROOT = PROJECT_ROOT / "fixtures" / "spiceextensionfixture"


def test_extension_entry_points_query_fixture_wheel_groups_without_importing(
    tmp_path,
):
    distribution = _fixture_distribution(tmp_path)

    discovered = {
        group: {
            entry.name: entry.value
            for entry in extension_entry_points(group, distributions=[distribution])
        }
        for group in SPICE_EXTENSION_ENTRY_POINT_GROUPS
    }

    assert discovered == {
        SPICE_DRIVER_ENTRY_POINT_GROUP: {
            "codex": "spiceextensiondriver:SHADOW_CODEX_DRIVER",
            "toy": "spiceextensiondriver:TOY_DRIVER",
        },
        SPICE_STUDY_ENTRY_POINT_GROUP: {
            "file-loc": "spiceextensionstudy:shadow_file_loc_study",
            "toy-study": "spiceextensionstudy:run_toy_study",
        },
        SPICE_WRAPPER_ENTRY_POINT_GROUP: {
            "spice-dev": "spiceextensionwrapper:shadow_spice_dev_wrapper_spec",
            "toy-wrapper": "spiceextensionwrapper:toy_wrapper_spec",
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
    distribution = _fixture_distribution(tmp_path)

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
    distribution = _fixture_distribution(tmp_path)

    with pytest.raises(SpiceError) as exc_info:
        extension_entry_points(
            SPICE_DRIVER_ENTRY_POINT_GROUP,
            distributions=[distribution, distribution],
        )

    assert str(exc_info.value) == (
        "duplicate extension entry point group 'spice.drivers' entry 'codex'; "
        "providers: "
        "spice-extension-fixture:spiceextensiondriver:SHADOW_CODEX_DRIVER, "
        "spice-extension-fixture:spiceextensiondriver:SHADOW_CODEX_DRIVER"
    )


def test_merge_builtin_and_extension_entry_points_keeps_builtins_first(tmp_path):
    distribution = _fixture_distribution(tmp_path)

    merged = merge_builtin_and_extension_entry_points(
        SPICE_DRIVER_ENTRY_POINT_GROUP,
        {"builtin": object()},
        distributions=[distribution],
    )

    assert tuple(merged) == ("builtin", "codex", "toy")


def test_agent_driver_registry_loads_toy_driver_from_fixture_wheel(
    tmp_path, monkeypatch
):
    wheel = _build_fixture_wheel(
        tmp_path,
        entry_points={
            SPICE_DRIVER_ENTRY_POINT_GROUP: {
                "toy": "spiceextensiondriver:TOY_DRIVER",
            }
        },
    )
    monkeypatch.syspath_prepend(str(wheel))
    monkeypatch.delenv(SPICE_AGENT_DRIVER_ENV, raising=False)

    config.update_section(
        tmp_path,
        config.AGENT_KEY,
        {config.AGENT_DRIVER_KEY: "toy"},
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
    wheel = _build_fixture_wheel(tmp_path)
    monkeypatch.syspath_prepend(str(wheel))

    with pytest.raises(SpiceError) as exc_info:
        select_driver("toy")

    message = str(exc_info.value)
    assert SPICE_DRIVER_ENTRY_POINT_GROUP in message
    assert "codex" in message
    assert "shadows built-in" in message


def _fixture_distribution(tmp_path: Path) -> metadata.Distribution:
    wheel = _build_fixture_wheel(tmp_path)
    distributions = list(metadata.distributions(path=[str(wheel)]))
    assert len(distributions) == 1
    return distributions[0]


def _build_fixture_wheel(
    tmp_path: Path, *, entry_points: dict[str, dict[str, str]] | None = None
) -> Path:
    pyproject = tomllib.loads(
        (FIXTURE_ROOT / "pyproject.toml").read_text(encoding="utf-8")
    )
    project = pyproject["project"]
    normalized_name = str(project["name"]).replace("-", "_")
    version = str(project["version"])
    dist_info = f"{normalized_name}-{version}.dist-info"
    wheel = tmp_path / f"{normalized_name}-{version}-py3-none-any.whl"

    records: list[str] = []
    with zipfile.ZipFile(wheel, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for source in sorted((FIXTURE_ROOT / "src").rglob("*")):
            if source.is_file() and source.suffix == ".py":
                _write_wheel_file(
                    archive,
                    records,
                    source.relative_to(FIXTURE_ROOT / "src").as_posix(),
                    source.read_bytes(),
                )
        _write_wheel_file(
            archive,
            records,
            f"{dist_info}/METADATA",
            _metadata_text(project).encode(),
        )
        _write_wheel_file(
            archive,
            records,
            f"{dist_info}/WHEEL",
            b"Wheel-Version: 1.0\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
        )
        _write_wheel_file(
            archive,
            records,
            f"{dist_info}/entry_points.txt",
            _entry_points_text(entry_points or project["entry-points"]).encode(),
        )
        archive.writestr(f"{dist_info}/RECORD", _record_text(records))
    return wheel


def _write_wheel_file(
    archive: zipfile.ZipFile, records: list[str], name: str, data: bytes
) -> None:
    archive.writestr(name, data)
    records.append(name)


def _metadata_text(project: dict[str, object]) -> str:
    return "\n".join(
        [
            "Metadata-Version: 2.3",
            f"Name: {project['name']}",
            f"Version: {project['version']}",
            f"Summary: {project['description']}",
            f"Requires-Python: {project['requires-python']}",
            "",
        ]
    )


def _entry_points_text(groups: dict[str, dict[str, str]]) -> str:
    lines: list[str] = []
    for group_name, entries in groups.items():
        lines.append(f"[{group_name}]")
        lines.extend(f"{name} = {target}" for name, target in entries.items())
        lines.append("")
    return "\n".join(lines)


def _record_text(records: list[str]) -> str:
    output = io.StringIO()
    writer = csv.writer(output, lineterminator="\n")
    for name in records:
        writer.writerow([name, "", ""])
    writer.writerow([records[-1].rsplit("/", 1)[0] + "/RECORD", "", ""])
    return output.getvalue()
