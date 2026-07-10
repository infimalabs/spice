"""Executable release-browser manifest and runner contracts."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BROWSER_DIR = ROOT / "tests" / "browser"
MANIFEST = BROWSER_DIR / "release_smoke_manifest.js"
RUNNER = BROWSER_DIR / "run_release_smokes.js"


def _load_manifest() -> dict[str, object]:
    script = "console.log(JSON.stringify(require(process.argv[1])))"
    result = subprocess.run(
        ["node", "-e", script, str(MANIFEST)],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def test_release_browser_manifest_classifies_every_smoke_once() -> None:
    manifest = _load_manifest()
    release_safe = [entry["path"] for entry in manifest["releaseSafe"]]
    external = [entry["path"] for entry in manifest["externalState"]]
    inventory = sorted(path.name for path in BROWSER_DIR.glob("*_smoke.js"))

    assert sorted(release_safe + external) == inventory
    assert len(set(release_safe + external)) == len(inventory)
    assert manifest["externalState"] == [
        {
            "path": "serve_task_card_live_smoke.js",
            "reason": (
                "creates a task through a bound live lane; run explicitly when "
                "validating live task-card delivery"
            ),
        }
    ]


def test_release_browser_runner_reports_scenario_output_and_exclusions(
    tmp_path: Path,
) -> None:
    passing = tmp_path / "passing_smoke.js"
    failing = tmp_path / "failing_smoke.js"
    excluded = tmp_path / "external_smoke.js"
    passing.write_text('console.log("passing output")\n', encoding="utf-8")
    failing.write_text(
        'console.error("actionable failure")\nprocess.exit(7)\n', encoding="utf-8"
    )
    excluded.write_text('console.log("external")\n', encoding="utf-8")
    manifest = tmp_path / "manifest.js"
    manifest.write_text(
        "module.exports = "
        + json.dumps(
            {
                "releaseSafe": [
                    {"path": passing.name},
                    {"path": failing.name},
                ],
                "externalState": [
                    {"path": excluded.name, "reason": "needs live state"}
                ],
            }
        )
        + ";\n",
        encoding="utf-8",
    )

    result = subprocess.run(
        ["node", str(RUNNER), str(manifest)],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 1
    assert "PASS passing_smoke.js" in result.stdout
    assert "SKIP external_smoke.js: needs live state" in result.stdout
    assert "FAIL failing_smoke.js" in result.stderr
    assert "actionable failure" in result.stderr
    assert "release browser gate failed" in result.stderr


def test_release_docs_require_repo_local_playwright_and_manifest() -> None:
    release_docs = (ROOT / "docs" / "release.md").read_text(encoding="utf-8")
    normalized = " ".join(release_docs.split())

    assert "tests/browser/release_smoke_manifest.js" in release_docs
    assert "Run `npm ci`" in release_docs
    assert "scratch-server and page-local fixtures are mandatory" in normalized
    assert "live external state" in release_docs
