from spice.cli.parser import build_parser
from spice.studies import cli as studies_cli
from spice.studies.complexity import (
    ComplexityRecord,
    complexity_hotspot_rows,
)


def test_complexity_hotspot_rows_rank_by_ccn_then_length_then_nloc():
    records = [
        _record("low", ccn=3, length=200, nloc=50),
        _record("wide", ccn=9, length=120, nloc=40),
        _record("dense", ccn=9, length=120, nloc=60),
        _record("peak", ccn=12, length=20, nloc=20),
    ]

    hotspots = complexity_hotspot_rows(records, limit=3)

    assert [record.function_name for record in hotspots] == ["peak", "dense", "wide"]


def test_complexity_hotspots_cli_uses_configured_default_limit(
    tmp_path, monkeypatch, capsys
):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.complexity]\nhotspot_limit = 1\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        studies_cli.complexity,
        "collect_complexity_records",
        lambda paths, *, root: [
            _record("first", ccn=9, length=10, nloc=10),
            _record("second", ccn=8, length=100, nloc=100),
        ],
    )
    args = build_parser().parse_args(["study", "complexity-hotspots", "src/app.py"])

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert "complexity-hotspots: top 1 of 2 routine(s)" in output
    assert "src/first.py:first" in output
    assert "src/second.py:second" not in output


def test_complexity_hotspots_cli_limit_overrides_config(tmp_path, monkeypatch, capsys):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.complexity]\nhotspot_limit = 1\n",
        encoding="utf-8",
    )
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    monkeypatch.setattr(
        studies_cli.complexity,
        "collect_complexity_records",
        lambda paths, *, root: [
            _record("first", ccn=9, length=10, nloc=10),
            _record("second", ccn=8, length=100, nloc=100),
        ],
    )
    args = build_parser().parse_args(
        ["study", "complexity-hotspots", "--limit", "2", "src/app.py"]
    )

    assert args.func(args) == 0
    output = capsys.readouterr().out
    assert "complexity-hotspots: top 2 of 2 routine(s)" in output
    assert "src/first.py:first" in output
    assert "src/second.py:second" in output


def _record(name: str, *, ccn: int, length: int, nloc: int) -> ComplexityRecord:
    return ComplexityRecord(
        path=f"src/{name}.py",
        function_name=name,
        ccn=ccn,
        length=length,
        nloc=nloc,
    )
