from spice.cli.parser import build_parser
from spice.serve.browser.artifacts import (
    SERVE_BROWSER_ARTIFACT_DIR,
    serve_browser_artifact_path,
)
from spice.serve.cli import run_serve_browser_artifact_path

ARTIFACT_FILENAME = "composer-smoke.png"
EXIT_OK = 0


def test_serve_browser_artifact_path_creates_dedicated_parent(tmp_path):
    path = serve_browser_artifact_path(ARTIFACT_FILENAME, root=tmp_path)

    assert path == tmp_path / SERVE_BROWSER_ARTIFACT_DIR / ARTIFACT_FILENAME
    assert path.parent.is_dir()


def test_serve_browser_artifact_path_cli_prints_destination(
    tmp_path, monkeypatch, capsys
):
    monkeypatch.chdir(tmp_path)
    args = build_parser().parse_args(
        ["serve", "browser-artifact-path", ARTIFACT_FILENAME]
    )

    result = args.func(args)

    assert args.func is run_serve_browser_artifact_path
    assert result == EXIT_OK
    assert capsys.readouterr().out.strip() == str(
        tmp_path / SERVE_BROWSER_ARTIFACT_DIR / ARTIFACT_FILENAME
    )
