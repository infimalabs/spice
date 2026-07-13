"""Serve speech backend rendering."""

from __future__ import annotations

import io
import subprocess
import sys
import wave
from pathlib import Path

import pytest

from spice import config
from spice.cli.parser import build_parser
from spice.configcli import handle_config
from spice.serve import audio

ESPEAK_TEST_SAMPLE_RATE = 8000


def test_default_speech_backend_uses_macos_say_config(tmp_path, monkeypatch):
    config.set_worktree_section(
        tmp_path,
        config.SAY_KEY,
        {
            config.SAY_VOICE_KEY: "Samantha",
            config.SAY_WORDS_PER_MINUTE_KEY: 200,
        },
    )
    seen: dict[str, object] = {}

    def fake_run(args, **kwargs):
        seen["args"] = args
        seen["input"] = kwargs["input"]
        output_path = Path(args[args.index("-o") + 1])
        output_path.write_bytes(b"m4a-bytes")
        return subprocess.CompletedProcess(args, 0)

    monkeypatch.setattr(audio.subprocess, "run", fake_run)

    rendered = audio.render_speech_audio(
        "hello/world",
        repo_root=tmp_path,
        rate_multiplier=1.5,
    )

    assert rendered == audio.SpeechAudio(b"m4a-bytes", "audio/mp4")
    assert seen["args"][:5] == ["say", "-v", "Samantha", "-r", "300"]
    assert seen["input"] == "hello world"


def test_external_speech_backend_uses_configured_command(tmp_path, monkeypatch):
    config.set_worktree_section(
        tmp_path,
        config.SAY_KEY,
        {
            config.SAY_BACKEND_KEY: "external",
            config.SAY_COMMAND_KEY: "tts-engine --wav",
            config.SAY_CONTENT_TYPE_KEY: "audio/wav",
        },
    )
    seen: dict[str, object] = {}

    def fake_run(args, **kwargs):
        seen["args"] = args
        seen["input"] = kwargs["input"]
        return subprocess.CompletedProcess(args, 0, stdout=b"wav-bytes", stderr=b"")

    monkeypatch.setattr(audio.subprocess, "run", fake_run)

    rendered = audio.render_speech_audio(
        "see [docs](https://example.test)",
        repo_root=tmp_path,
    )

    assert rendered == audio.SpeechAudio(b"wav-bytes", "audio/wav")
    assert seen["args"] == ["tts-engine", "--wav"]
    assert seen["input"] == b"see docs"


def test_external_speech_backend_reports_command_failure(tmp_path, monkeypatch):
    config.set_worktree_section(
        tmp_path,
        config.SAY_KEY,
        {
            config.SAY_BACKEND_KEY: "external",
            config.SAY_COMMAND_KEY: "tts-engine",
        },
    )

    def fake_run(args, **kwargs):
        return subprocess.CompletedProcess(args, 7, stdout=b"", stderr=b"bad model")

    monkeypatch.setattr(audio.subprocess, "run", fake_run)

    with pytest.raises(
        RuntimeError,
        match="external speech backend exited 7: bad model",
    ):
        audio.render_speech_audio("hello", repo_root=tmp_path)


def test_espeak_ng_stdout_recipe_runs_end_to_end(tmp_path, monkeypatch, capsys):
    executable_dir = tmp_path / "bin"
    executable_dir.mkdir()
    executable = executable_dir / "espeak-ng"
    executable.write_text(
        f"#!{sys.executable}\n"
        "import sys\n"
        "import wave\n"
        "from pathlib import Path\n"
        "text = sys.stdin.buffer.read()\n"
        "Path('espeak-ng.stdin').write_bytes(text)\n"
        "with wave.open(sys.stdout.buffer, 'wb') as output:\n"
        "    output.setnchannels(1)\n"
        "    output.setsampwidth(2)\n"
        f"    output.setframerate({ESPEAK_TEST_SAMPLE_RATE})\n"
        "    output.writeframes(b'\\x00\\x00')\n",
        encoding="utf-8",
    )
    executable.chmod(0o755)
    monkeypatch.setenv("PATH", str(executable_dir))
    monkeypatch.chdir(tmp_path)
    monkeypatch.setattr("spice.configcli.require_repo_root", lambda: tmp_path)

    result = handle_config(
        build_parser().parse_args(
            [
                "config",
                "say",
                "--backend",
                "external",
                "--command",
                "espeak-ng --stdout",
                "--content-type",
                "audio/wav",
            ]
        )
    )
    rendered = audio.render_speech_audio(
        "see [Linux docs](https://example.test)/today",
        repo_root=tmp_path,
    )

    assert result == 0
    assert capsys.readouterr().out == (
        "say backend=external command=espeak-ng --stdout content_type=audio/wav\n"
    )
    assert (tmp_path / "espeak-ng.stdin").read_bytes() == b"see Linux docs today"
    assert rendered.content_type == "audio/wav"
    with wave.open(io.BytesIO(rendered.data), "rb") as wav_file:
        assert wav_file.getnchannels() == 1
        assert wav_file.getsampwidth() == 2
        assert wav_file.getframerate() == ESPEAK_TEST_SAMPLE_RATE
        assert wav_file.getnframes() == 1
        assert wav_file.getcomptype() == "NONE"


def test_espeak_ng_linux_preset_is_documented():
    overview = Path("CONFIG.md").read_text(encoding="utf-8")
    reference = Path("docs/config/reference.md").read_text(encoding="utf-8")

    assert (
        "[`espeak-ng` preset](docs/config/reference.md#linux-speech-with-espeak-ng)"
        in overview
    )
    assert "sudo apt-get install espeak-ng" in reference
    assert "command -v espeak-ng" in reference
    assert "espeak-ng --version" in reference
    assert (
        'spice config say --backend external --command "espeak-ng --stdout" '
        "--content-type audio/wav" in reference
    )
    assert "returned on stdout as `audio/wav`" in reference
