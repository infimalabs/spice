from pathlib import Path

from spice.policyconfig import resolve_policy
from spice.studies.fileloc import scan_loc_violations
from spice.studies.magicnums import detect_magic_regressions


def test_configured_lockfiles_do_not_trip_file_shape_pressure(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.lockfiles]\n"
        'suffixes = [".customlock"]\n'
        'names = ["custom-lock.json"]\n',
        encoding="utf-8",
    )
    suffix_lock_path = Path("tool.customlock")
    named_lock_path = Path("client") / "custom-lock.json"
    source_path = Path("large_source.py")
    (tmp_path / "client").mkdir()
    (tmp_path / suffix_lock_path).write_text("state = []\n" * 20, encoding="utf-8")
    (tmp_path / named_lock_path).write_text("state = []\n" * 20, encoding="utf-8")
    (tmp_path / source_path).write_text("print('large')\n" * 20, encoding="utf-8")
    resolved = resolve_policy(tmp_path)

    findings = scan_loc_violations(
        [suffix_lock_path, named_lock_path, source_path],
        root=tmp_path,
        limit=10,
        flex_limit_value=10,
        byte_limit=100,
        byte_flex_limit_value=100,
        lockfile_suffixes=resolved.lockfiles.suffixes,
        lockfile_names=resolved.lockfiles.names,
    )

    assert [finding.path for finding in findings] == [source_path.as_posix()]


def test_configured_magic_c_grammar_suffix_is_scanned(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy.languages]\nmagic = [".wat"]\nc_grammar = [".wat"]\n',
        encoding="utf-8",
    )
    rel_path = Path("sample.wat")
    (tmp_path / rel_path).write_text(
        "if (delta > 75) {\n  grow();\n}\n", encoding="utf-8"
    )
    resolved = resolve_policy(tmp_path)

    findings = detect_magic_regressions(
        [rel_path],
        root=tmp_path,
        baseline_ref=resolved.magic.baseline_ref,
        suffixes=resolved.languages.magic,
        c_grammar_suffixes=resolved.languages.c_grammar,
    )

    assert [(finding.line, finding.literal) for finding in findings] == [(1, "75")]
