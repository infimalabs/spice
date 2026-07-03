from __future__ import annotations

import base64
import csv
import hashlib
import io
import tomllib
import zipfile
from importlib import metadata
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
FIXTURE_ROOT = PROJECT_ROOT / "fixtures" / "spiceextensionfixture"


def test_extension_fixture_wheel_exposes_real_entry_points(tmp_path, monkeypatch):
    wheel = _build_fixture_wheel(FIXTURE_ROOT, tmp_path)

    distribution = _single_distribution_from_wheel(wheel)
    entry_points = _entry_point_map(distribution)

    assert distribution.metadata["Name"] == "spice-extension-fixture"
    assert distribution.metadata["Version"] == "0.1.0"
    assert sorted(entry_points) == [
        "spice.drivers",
        "spice.studies",
        "spice.wrappers",
    ]
    assert sorted(entry_points["spice.drivers"]) == ["codex", "toy"]
    assert sorted(entry_points["spice.studies"]) == ["file-loc", "toy-study"]
    assert sorted(entry_points["spice.wrappers"]) == ["spice-dev", "toy-wrapper"]

    monkeypatch.syspath_prepend(str(wheel))
    toy_driver = entry_points["spice.drivers"]["toy"].load()
    shadow_driver = entry_points["spice.drivers"]["codex"].load()
    toy_study = entry_points["spice.studies"]["toy-study"].load()
    shadow_study = entry_points["spice.studies"]["file-loc"].load()
    toy_wrapper = entry_points["spice.wrappers"]["toy-wrapper"].load()
    shadow_wrapper = entry_points["spice.wrappers"]["spice-dev"].load()

    assert toy_driver.name == "toy"
    assert toy_driver.build_exec_command(
        repo_root=tmp_path,
        prompt="hello",
        thread_id="thread-1",
        model="toy-large",
    ) == ["toy-agent", "exec", "--thread", "thread-1", "--model", "toy-large", "hello"]
    assert shadow_driver.name == "codex"
    assert toy_study(["a.py"]) == {"study": "toy", "paths": ["a.py"]}
    assert shadow_study(["b.py"]) == {"study": "file-loc", "paths": ["b.py"]}
    assert toy_wrapper() == {"argv": ["toy-wrapper-bin", "--from-entry-point"]}
    assert shadow_wrapper() == {"argv": ["shadow-spice-dev", "--from-entry-point"]}


def _single_distribution_from_wheel(wheel: Path) -> metadata.Distribution:
    distributions = list(metadata.distributions(path=[str(wheel)]))
    assert len(distributions) == 1
    return distributions[0]


def _entry_point_map(
    distribution: metadata.Distribution,
) -> dict[str, dict[str, metadata.EntryPoint]]:
    grouped: dict[str, dict[str, metadata.EntryPoint]] = {}
    for entry_point in distribution.entry_points:
        grouped.setdefault(entry_point.group, {})[entry_point.name] = entry_point
    return grouped


def _build_fixture_wheel(fixture_root: Path, tmp_path: Path) -> Path:
    pyproject = tomllib.loads(
        (fixture_root / "pyproject.toml").read_text(encoding="utf-8")
    )
    project = pyproject["project"]
    normalized_name = _normalized_distribution_name(project["name"])
    version = project["version"]
    dist_info = f"{normalized_name}-{version}.dist-info"
    wheel = tmp_path / f"{normalized_name}-{version}-py3-none-any.whl"

    records: list[tuple[str, bytes]] = []
    with zipfile.ZipFile(wheel, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for source in sorted((fixture_root / "src").rglob("*")):
            if not source.is_file() or source.suffix != ".py":
                continue
            archive_name = source.relative_to(fixture_root / "src").as_posix()
            _write_wheel_file(archive, records, archive_name, source.read_bytes())

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
            "\n".join(
                [
                    "Wheel-Version: 1.0",
                    "Generator: spice fixture wheel builder",
                    "Root-Is-Purelib: true",
                    "Tag: py3-none-any",
                    "",
                ]
            ).encode(),
        )
        _write_wheel_file(
            archive,
            records,
            f"{dist_info}/entry_points.txt",
            _entry_points_text(project["entry-points"]).encode(),
        )
        archive.writestr(
            f"{dist_info}/RECORD",
            _record_text(records, f"{dist_info}/RECORD"),
        )

    return wheel


def _write_wheel_file(
    archive: zipfile.ZipFile, records: list[tuple[str, bytes]], name: str, data: bytes
) -> None:
    archive.writestr(name, data)
    records.append((name, data))


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
        for name, target in entries.items():
            lines.append(f"{name} = {target}")
        lines.append("")
    return "\n".join(lines)


def _record_text(records: list[tuple[str, bytes]], record_name: str) -> str:
    output = io.StringIO()
    writer = csv.writer(output, lineterminator="\n")
    for name, data in records:
        writer.writerow([name, f"sha256={_urlsafe_sha256(data)}", str(len(data))])
    writer.writerow([record_name, "", ""])
    return output.getvalue()


def _urlsafe_sha256(data: bytes) -> str:
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).decode()
    return digest.rstrip("=")


def _normalized_distribution_name(name: str) -> str:
    return name.replace("-", "_")
