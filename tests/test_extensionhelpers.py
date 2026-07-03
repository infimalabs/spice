from __future__ import annotations

import base64
import csv
import hashlib
import io
import tomllib
import zipfile
from collections.abc import Mapping
from importlib import metadata
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
FIXTURE_ROOT = PROJECT_ROOT / "fixtures" / "spiceextensionfixture"


class FilteredExtensionDistribution:
    def __init__(
        self,
        distribution: metadata.Distribution,
        names_by_group: Mapping[str, set[str]],
    ) -> None:
        self._distribution = distribution
        self._names_by_group = names_by_group

    @property
    def metadata(self) -> metadata.PackageMetadata:
        return self._distribution.metadata

    @property
    def entry_points(self) -> tuple[metadata.EntryPoint, ...]:
        return tuple(
            entry_point
            for entry_point in self._distribution.entry_points
            if entry_point.group in self._names_by_group
            and entry_point.name in self._names_by_group[entry_point.group]
        )


def build_fixture_distribution(
    tmp_path: Path,
    *,
    entry_points: Mapping[str, Mapping[str, str]] | None = None,
) -> tuple[Path, metadata.Distribution]:
    wheel = build_fixture_wheel(tmp_path, entry_points=entry_points)
    return wheel, single_distribution_from_wheel(wheel)


def build_fixture_wheel(
    tmp_path: Path,
    *,
    fixture_root: Path = FIXTURE_ROOT,
    entry_points: Mapping[str, Mapping[str, str]] | None = None,
) -> Path:
    pyproject = tomllib.loads(
        (fixture_root / "pyproject.toml").read_text(encoding="utf-8")
    )
    project = pyproject["project"]
    normalized_name = _normalized_distribution_name(project["name"])
    version = str(project["version"])
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
        configured_entry_points = (
            entry_points if entry_points is not None else project["entry-points"]
        )
        _write_wheel_file(
            archive,
            records,
            f"{dist_info}/entry_points.txt",
            _entry_points_text(configured_entry_points).encode(),
        )
        archive.writestr(
            f"{dist_info}/RECORD",
            _record_text(records, f"{dist_info}/RECORD"),
        )

    return wheel


def single_distribution_from_wheel(wheel: Path) -> metadata.Distribution:
    distributions = list(metadata.distributions(path=[str(wheel)]))
    assert len(distributions) == 1
    return distributions[0]


def entry_point_map(
    distribution: metadata.Distribution,
) -> dict[str, dict[str, metadata.EntryPoint]]:
    grouped: dict[str, dict[str, metadata.EntryPoint]] = {}
    for entry_point in distribution.entry_points:
        grouped.setdefault(entry_point.group, {})[entry_point.name] = entry_point
    return grouped


def _write_wheel_file(
    archive: zipfile.ZipFile, records: list[tuple[str, bytes]], name: str, data: bytes
) -> None:
    archive.writestr(name, data)
    records.append((name, data))


def _metadata_text(project: Mapping[str, object]) -> str:
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


def _entry_points_text(groups: Mapping[str, Mapping[str, str]]) -> str:
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


def _normalized_distribution_name(name: object) -> str:
    return str(name).replace("-", "_")
