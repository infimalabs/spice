"""Executable release-browser manifest and runner contracts."""

from __future__ import annotations

import json
import os
import subprocess
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parent.parent
BROWSER_DIR = ROOT / "tests" / "browser"
MANIFEST = BROWSER_DIR / "release_smoke_manifest.js"
RUNNER = BROWSER_DIR / "run_release_smokes.js"
HARNESS = BROWSER_DIR / "serve_playwright_harness.js"
IMPORT_SHELL_SMOKE = BROWSER_DIR / "serve_fresh_startup_import_shell_smoke.js"
CALLER_BUDGET_MS = 45000
REPLACED_IMPORT_SHELL_LITERAL_MS = 10000
UNSETTLED_BUDGET_MS = 250
UNSETTLED_CEILING_MS = 5000


def _node(script: str, *arguments: str) -> dict[str, Any]:
    result = subprocess.run(
        ["node", "-e", script, *arguments],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return json.loads(result.stdout)


def _load_manifest() -> dict[str, Any]:
    return _node("console.log(JSON.stringify(require(process.argv[1])))", str(MANIFEST))


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
        env={
            **os.environ,  # env-policy: allow
            "SPICE_RELEASE_BROWSER_REPORT": str(tmp_path / "browser-report.json"),
        },
        text=True,
    )
    report = json.loads((tmp_path / "browser-report.json").read_text(encoding="utf-8"))

    assert result.returncode == 1
    assert "PASS passing_smoke.js" in result.stdout
    assert "SKIP external_smoke.js: needs live state" in result.stdout
    assert "FAIL failing_smoke.js" in result.stderr
    assert "actionable failure" in result.stderr
    assert "release browser gate failed" in result.stderr
    assert report == {
        "schemaVersion": 1,
        "counts": {"failed": 1, "passed": 1, "skipped": 1, "total": 3},
        "externalState": [{"path": "external_smoke.js", "reason": "needs live state"}],
        "scenarios": [
            {"path": "passing_smoke.js", "serial": False, "status": "passed"},
            {"path": "failing_smoke.js", "serial": False, "status": "failed"},
        ],
    }


def test_import_shell_wait_takes_the_harness_lifecycle_budget() -> None:
    budgets = _node(
        "const harness = require(process.argv[1]);"
        "const smoke = require(process.argv[2]);"
        "const caller = Number(process.argv[3]);"
        "console.log(JSON.stringify({"
        "  derived: smoke.importShellTimeoutMs(),"
        "  harness: harness.defaultLifecycleReadyTimeoutMs,"
        "  overridden: smoke.importShellTimeoutMs({lifecycleReadyTimeoutMs: caller}),"
        "}));",
        str(HARNESS),
        str(IMPORT_SHELL_SMOKE),
        str(CALLER_BUDGET_MS),
    )

    assert budgets["derived"] == budgets["harness"]
    assert budgets["overridden"] == CALLER_BUDGET_MS
    assert budgets["harness"] > REPLACED_IMPORT_SHELL_LITERAL_MS


def test_import_shell_wait_fails_promptly_when_the_shell_never_settles() -> None:
    outcome = _node(
        "const smoke = require(process.argv[1]);"
        "const page = {"
        "  waitForFunction(predicate, argument, options) {"
        "    return new Promise((resolve, reject) => {"
        "      setTimeout(() => {"
        "        reject(new Error("
        "          'page.waitForFunction: Timeout ' + options.timeout + 'ms exceeded'"
        "        ));"
        "      }, options.timeout);"
        "    });"
        "  },"
        "};"
        "const budget = Number(process.argv[2]);"
        "const started = Date.now();"
        "smoke.waitForImportShell(page, {lifecycleReadyTimeoutMs: budget}).then("
        "  () => { console.log(JSON.stringify({settled: true})); },"
        "  (error) => {"
        "    console.log(JSON.stringify({"
        "      settled: false,"
        "      message: error.message,"
        "      elapsedMs: Date.now() - started,"
        "    }));"
        "  },"
        ");",
        str(IMPORT_SHELL_SMOKE),
        str(UNSETTLED_BUDGET_MS),
    )

    assert outcome["settled"] is False
    assert outcome["message"] == (
        f"page.waitForFunction: Timeout {UNSETTLED_BUDGET_MS}ms exceeded"
    )
    assert UNSETTLED_BUDGET_MS <= outcome["elapsedMs"] < UNSETTLED_CEILING_MS


def test_release_docs_require_repo_local_playwright_and_manifest() -> None:
    release_docs = (ROOT / "docs" / "release.md").read_text(encoding="utf-8")
    normalized = " ".join(release_docs.split())

    assert "tests/browser/release_smoke_manifest.js" in release_docs
    assert "Run `npm ci`" in release_docs
    assert "scratch-server and page-local fixtures are mandatory" in normalized
    assert "live external state" in release_docs
