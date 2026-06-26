from pathlib import Path

from spice.cli.parser import build_parser
from spice.studies import cli as studies_cli
from spice.studies.reachability import (
    ReachabilityFinding,
    render_reachability_board,
    render_symbol_reachability_board,
    scan_reachability,
    scan_symbol_reachability,
)


def test_reachability_scans_test_files_outside_package_root(tmp_path):
    _write_reachability_repo(tmp_path, "import spice.onlytest\n")

    findings = scan_reachability(tmp_path)

    assert [(f.module, f.module_path, f.only_test_imports) for f in findings] == [
        ("spice.onlytest", "spice/onlytest.py", ["test_only.py"])
    ]


def test_reachability_expands_from_imported_submodule(tmp_path):
    _write_reachability_repo(tmp_path, "from spice import onlytest\n")

    findings = scan_reachability(tmp_path)

    assert [(f.module, f.module_path, f.only_test_imports) for f in findings] == [
        ("spice.onlytest", "spice/onlytest.py", ["test_only.py"])
    ]


def test_symbol_reachability_excludes_production_used_local_helpers(tmp_path):
    _write_symbol_reachability_repo(tmp_path)

    module_findings = scan_reachability(tmp_path)
    symbol_findings = scan_symbol_reachability(tmp_path)
    module_output = "\n".join(render_reachability_board(module_findings))
    symbol_output = "\n".join(render_symbol_reachability_board(symbol_findings))

    assert "reachability: 1 test-only module(s)" in module_output
    assert "spice/orphan_module_xyz.py" in module_output
    assert "symbol-reachability: 2 test-only symbol(s)" in symbol_output
    assert "spice/live.py:LiveThing.planted_dead_method_abc" in symbol_output
    assert "spice/live.py:planted_dead_function_abc" in symbol_output
    assert "handle_one_request" not in symbol_output
    assert "shared_helper" not in symbol_output
    assert "shared_method" not in symbol_output


def test_symbol_reachability_resolves_registry_literal_dispatch(tmp_path):
    """A symbol reached only through a registry dict/list literal in a
    production module is a real production reference: the scanner sees the
    symbol named as a literal value, so registry-dispatched handlers are not
    false-flagged as test-only the way getattr-by-constructed-string would be.
    """
    (tmp_path / "spice" / "cli").mkdir(parents=True)
    (tmp_path / "tests").mkdir()
    (tmp_path / "spice" / "cli" / "entry.py").write_text(
        "from ..registry import DISPATCH, ORDERED\n"
        "def main(key):\n"
        "    DISPATCH[key]()\n"
        "    return ORDERED[0]()\n",
        encoding="utf-8",
    )
    (tmp_path / "spice" / "registry.py").write_text(
        "from .handlers import handle_dict_only, handle_list_only\n"
        "DISPATCH = {'one': handle_dict_only}\n"
        "ORDERED = [handle_list_only]\n",
        encoding="utf-8",
    )
    (tmp_path / "spice" / "handlers.py").write_text(
        "def handle_dict_only():\n    return 1\n\n"
        "def handle_list_only():\n    return 2\n\n"
        "def handle_orphan():\n    return 3\n",
        encoding="utf-8",
    )
    (tmp_path / "tests" / "test_handlers.py").write_text(
        "from spice.handlers import (\n"
        "    handle_dict_only,\n"
        "    handle_list_only,\n"
        "    handle_orphan,\n"
        ")\n"
        "def test_handlers():\n"
        "    assert handle_dict_only() == 1\n"
        "    assert handle_list_only() == 2\n"
        "    assert handle_orphan() == 3\n",
        encoding="utf-8",
    )

    findings = scan_symbol_reachability(tmp_path)

    flagged = {f.symbol for f in findings}
    assert flagged == {"handle_orphan"}


def test_symbol_reachability_resolves_typed_parameter_method_calls(tmp_path):
    (tmp_path / "spice" / "cli").mkdir(parents=True)
    (tmp_path / "spice" / "serve" / "team").mkdir(parents=True)
    (tmp_path / "tests").mkdir()
    (tmp_path / "spice" / "cli" / "entry.py").write_text(
        "from spice.helper import create_default_team\n"
        "from spice.serve.team.store import ServeTeamStore\n"
        "def main():\n"
        "    return create_default_team(ServeTeamStore())\n",
        encoding="utf-8",
    )
    (tmp_path / "spice" / "helper.py").write_text(
        "from spice.serve.team.store import ServeTeamStore\n"
        "def create_default_team(team_store: ServeTeamStore):\n"
        "    team_store.create_team()\n"
        "    cached_store: ServeTeamStore = team_store\n"
        "    return cached_store.rename_team()\n"
        "def create_from_constructor_assignment():\n"
        "    assigned_store = ServeTeamStore()\n"
        "    return assigned_store.constructor_only_method()\n",
        encoding="utf-8",
    )
    (tmp_path / "spice" / "serve" / "team" / "store.py").write_text(
        "class ServeTeamStore:\n"
        "    def create_team(self):\n"
        "        return 'created'\n\n"
        "    def rename_team(self):\n"
        "        return 'renamed'\n\n"
        "    def constructor_only_method(self):\n"
        "        return 'assigned'\n\n"
        "    def test_only_method(self):\n"
        "        return 'test-only'\n",
        encoding="utf-8",
    )
    (tmp_path / "tests" / "test_store.py").write_text(
        "from spice.serve.team.store import ServeTeamStore\n"
        "def test_store_methods():\n"
        "    assert ServeTeamStore().create_team() == 'created'\n"
        "    assert ServeTeamStore().rename_team() == 'renamed'\n"
        "    assert ServeTeamStore().constructor_only_method() == 'assigned'\n"
        "    assert ServeTeamStore().test_only_method() == 'test-only'\n",
        encoding="utf-8",
    )

    findings = scan_symbol_reachability(tmp_path)

    flagged = {f.symbol for f in findings}
    assert flagged == {"ServeTeamStore.test_only_method"}


def test_symbol_reachability_allowlist_exempts_qualified_symbol(tmp_path):
    _write_symbol_reachability_repo(tmp_path)

    findings = scan_symbol_reachability(
        tmp_path, allowlist=["spice.live.planted_dead_function_abc"]
    )
    output = "\n".join(render_symbol_reachability_board(findings))

    assert "symbol-reachability: 1 test-only symbol(s)" in output
    assert "planted_dead_function_abc" not in output
    assert "spice/live.py:LiveThing.planted_dead_method_abc" in output


def test_symbol_reachability_allowlist_exempts_whole_module(tmp_path):
    _write_symbol_reachability_repo(tmp_path)

    findings = scan_symbol_reachability(tmp_path, allowlist=["spice.live"])

    assert findings == []


def test_study_symbol_reachability_cli_reports_test_only_symbol(
    tmp_path, monkeypatch, capsys
):
    _write_symbol_reachability_repo(tmp_path)
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "symbol-reachability"])

    assert args.func(args) == 1
    output = capsys.readouterr().out
    assert "symbol-reachability: 2 test-only symbol(s)" in output
    assert "spice/live.py:planted_dead_function_abc" in output
    assert "spice.live.planted_dead_function_abc (function)" in output


def test_study_reachability_cli_reports_test_only_module(tmp_path, monkeypatch, capsys):
    _write_reachability_repo(tmp_path, "from spice import onlytest\n")
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    args = build_parser().parse_args(["study", "reachability"])

    assert args.func(args) == 1
    output = capsys.readouterr().out
    assert "reachability: 1 test-only module(s)" in output
    assert "spice/onlytest.py" in output
    assert "module: spice.onlytest" in output


def test_study_reachability_cli_create_tasks_passes_findings(
    tmp_path, monkeypatch, capsys
):
    _write_reachability_repo(tmp_path, "from spice import onlytest\n")
    monkeypatch.setattr(studies_cli, "require_repo_root", lambda: tmp_path)
    created_paths: list[str] = []
    monkeypatch.setattr(
        studies_cli,
        "_create_exhaust_tasks",
        lambda findings: created_paths.extend(f.module_path for f in findings),
    )
    args = build_parser().parse_args(["study", "reachability", "--create-tasks"])

    assert args.func(args) == 1

    output = capsys.readouterr().out
    assert "reachability: 1 test-only module(s)" in output
    assert created_paths == ["spice/onlytest.py"]


def test_create_exhaust_tasks_adds_decision_metadata_for_each_finding(
    monkeypatch, capsys
):
    from spice.tasks import create

    created: list[dict[str, object]] = []

    def fake_add(
        title: str,
        *,
        project: str,
        tags: list[str],
        acceptance: list[str],
    ) -> str:
        created.append(
            {
                "title": title,
                "project": project,
                "tags": tags,
                "acceptance": acceptance,
            }
        )
        return f"EXHAUST-{len(created)}"

    monkeypatch.setattr(create, "add", fake_add)

    studies_cli._create_exhaust_tasks(
        [
            ReachabilityFinding(
                module="spice.onlytest",
                module_path="spice/onlytest.py",
                only_test_imports=["tests/test_only.py"],
            ),
            ReachabilityFinding(
                module="spice.empty",
                module_path="spice/empty.py",
                only_test_imports=[],
            ),
        ]
    )

    assert created == [
        {
            "title": "Exhaust decision: wire-in/delete-both spice/onlytest.py",
            "project": "tests.exhaust",
            "tags": ["exhaust", "decision", "wire_in_delete_both"],
            "acceptance": [
                "Resolve spice.onlytest by either wiring it into a production "
                "entry point or deleting spice/onlytest.py along with every "
                "test that imports it.",
                "Current test-only importers: tests/test_only.py.",
            ],
        },
        {
            "title": "Exhaust decision: wire-in/delete-both spice/empty.py",
            "project": "tests.exhaust",
            "tags": ["exhaust", "decision", "wire_in_delete_both"],
            "acceptance": [
                "Resolve spice.empty by either wiring it into a production "
                "entry point or deleting spice/empty.py along with every test "
                "that imports it.",
                "Current test-only importers: unknown.",
            ],
        },
    ]
    assert capsys.readouterr().out == (
        "  task created: EXHAUST-1\n  task created: EXHAUST-2\n"
    )


def test_reachability_merges_default_allowlist(tmp_path):
    _write_reachability_repo(tmp_path, "import spice.release\n", module_name="release")

    assert scan_reachability(tmp_path) == []


def _write_reachability_repo(
    root: Path, test_import: str, *, module_name: str = "onlytest"
) -> None:
    (root / "spice" / "cli").mkdir(parents=True)
    (root / "tests").mkdir()
    (root / "spice" / "cli" / "entry.py").write_text("", encoding="utf-8")
    (root / "spice" / f"{module_name}.py").write_text("VALUE = 1\n", encoding="utf-8")
    (root / "tests" / "test_only.py").write_text(test_import, encoding="utf-8")


def _write_symbol_reachability_repo(root: Path) -> None:
    (root / "spice" / "cli").mkdir(parents=True)
    (root / "tests").mkdir()
    (root / "spice" / "cli" / "entry.py").write_text(
        "from ..live import production_function, LiveHandler, LiveThing\n"
        "production_function()\n"
        "LiveThing().production_method()\n"
        "LiveHandler\n",
        encoding="utf-8",
    )
    (root / "spice" / "live.py").write_text(
        "from http.server import BaseHTTPRequestHandler\n\n"
        "def production_function():\n"
        "    return shared_helper()\n\n"
        "def shared_helper():\n"
        "    return 1\n\n"
        "def planted_dead_function_abc():\n"
        "    return 2\n\n"
        "class LiveThing:\n"
        "    def production_method(self):\n"
        "        return self.shared_method()\n\n"
        "    def shared_method(self):\n"
        "        return 3\n\n"
        "    def planted_dead_method_abc(self):\n"
        "        return 4\n"
        "\n"
        "class LiveHandler(BaseHTTPRequestHandler):\n"
        "    def handle_one_request(self):\n"
        "        return None\n",
        encoding="utf-8",
    )
    (root / "spice" / "orphan_module_xyz.py").write_text(
        "def only_tests_call():\n    return 5\n", encoding="utf-8"
    )
    (root / "tests" / "test_symbols.py").write_text(
        "from spice.live import LiveHandler, LiveThing, planted_dead_function_abc, shared_helper\n"
        "import spice.orphan_module_xyz\n\n"
        "def test_symbols():\n"
        "    shared_helper()\n"
        "    planted_dead_function_abc()\n"
        "    LiveHandler.handle_one_request\n"
        "    LiveThing().shared_method()\n"
        "    LiveThing().planted_dead_method_abc()\n",
        encoding="utf-8",
    )
