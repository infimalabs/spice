"""Serve speech backend rendering."""

from __future__ import annotations

import io
import subprocess
import sys
import time
import wave
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

import pytest

from spice import config
from spice.cli.parser import build_parser
from spice.configcli import handle_config
from spice.procs import ProcessDeadlineExceeded
from spice.serve import audio

ESPEAK_TEST_SAMPLE_RATE = 8000
LONG_MESSAGE_FLOOR_SECONDS = 60.0


@dataclass(frozen=True)
class SpeechDeadlineOutcome:
    state: str
    phase: str
    input_label: str
    elapsed_seconds: float


def _speech_deadline_outcome(
    operation: Callable[[], object],
) -> SpeechDeadlineOutcome:
    started = time.monotonic()
    try:
        operation()
    except ProcessDeadlineExceeded as exc:
        return SpeechDeadlineOutcome(
            "timed-out",
            exc.phase,
            exc.input_label,
            time.monotonic() - started,
        )
    return SpeechDeadlineOutcome(
        "completed", "completed", "completed", time.monotonic() - started
    )


def test_default_speech_backend_uses_macos_say_config(tmp_path, monkeypatch):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {
            config.SAY_VOICE_KEY: "Samantha",
            config.SAY_WORDS_PER_MINUTE_KEY: 200,
        },
    )
    seen: dict[str, object] = {}

    def fake_run(args, **kwargs):
        seen["args"] = args
        seen["input"] = kwargs["input_data"]
        seen["timeout"] = kwargs["timeout_seconds"]
        output_path = Path(args[args.index("-o") + 1])
        output_path.write_bytes(b"m4a-bytes")
        return subprocess.CompletedProcess(args, 0)

    monkeypatch.setattr(audio, "run_bounded_process_group", fake_run)

    rendered = audio.render_speech_audio(
        "hello/world",
        repo_root=tmp_path,
        rate_multiplier=1.5,
    )

    assert rendered == audio.SpeechAudio(b"m4a-bytes", "audio/mp4")
    assert seen["args"][:5] == ["say", "-v", "Samantha", "-r", "300"]
    assert seen["input"] == "hello world"
    assert seen["timeout"] == config.DEFAULT_SAY_TIMEOUT_SECONDS


def test_external_speech_backend_uses_configured_command(tmp_path, monkeypatch):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
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
        seen["input"] = kwargs["input_data"]
        seen["phase"] = kwargs["phase"]
        return subprocess.CompletedProcess(args, 0, stdout=b"wav-bytes", stderr=b"")

    monkeypatch.setattr(audio, "run_bounded_process_group", fake_run)

    rendered = audio.render_speech_audio(
        "see [docs](https://example.test)",
        repo_root=tmp_path,
    )

    assert rendered == audio.SpeechAudio(b"wav-bytes", "audio/wav")
    assert seen["args"] == ["tts-engine", "--wav"]
    assert seen["input"] == b"see docs"
    assert seen["phase"] == "serve-speech-external"


def test_external_speech_backend_reports_command_failure(tmp_path, monkeypatch):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {
            config.SAY_BACKEND_KEY: "external",
            config.SAY_COMMAND_KEY: "tts-engine",
        },
    )

    def fake_run(args, **kwargs):
        return subprocess.CompletedProcess(args, 7, stdout=b"", stderr=b"bad model")

    monkeypatch.setattr(audio, "run_bounded_process_group", fake_run)

    with pytest.raises(
        RuntimeError,
        match="external speech backend exited 7: bad model",
    ):
        audio.render_speech_audio("hello", repo_root=tmp_path)


def test_external_speech_timeout_is_configurable(tmp_path, monkeypatch):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {
            config.SAY_BACKEND_KEY: "external",
            config.SAY_COMMAND_KEY: "tts-engine",
            config.SAY_TIMEOUT_SECONDS_KEY: 0.25,
        },
    )
    seen: dict[str, object] = {}

    def fake_run(args, **kwargs):
        seen["timeout"] = kwargs["timeout_seconds"]
        return subprocess.CompletedProcess(args, 0, stdout=b"wav-bytes", stderr=b"")

    monkeypatch.setattr(audio, "run_bounded_process_group", fake_run)

    audio.render_speech_audio("hello", repo_root=tmp_path)

    assert seen["timeout"] == 0.25


def test_long_message_renders_within_the_generous_default_bound(tmp_path, monkeypatch):
    long_message = "word " * 200
    seen: dict[str, object] = {}

    def fake_run(args, **kwargs):
        seen["timeout"] = kwargs["timeout_seconds"]
        output_path = Path(args[args.index("-o") + 1])
        output_path.write_bytes(b"m4a-bytes")
        return subprocess.CompletedProcess(args, 0)

    monkeypatch.setattr(audio, "run_bounded_process_group", fake_run)

    rendered = audio.render_speech_audio(long_message, repo_root=tmp_path)

    assert rendered == audio.SpeechAudio(b"m4a-bytes", "audio/mp4")
    assert seen["timeout"] == config.DEFAULT_SAY_TIMEOUT_SECONDS
    assert seen["timeout"] > LONG_MESSAGE_FLOOR_SECONDS


def test_stalled_external_speech_releases_worker_with_named_deadline(tmp_path):
    config.set_scope_section(
        tmp_path,
        config.WORKTREE_SOURCE,
        config.SAY_KEY,
        {
            config.SAY_BACKEND_KEY: "external",
            config.SAY_COMMAND_KEY: (
                f'{sys.executable} -c "import time; time.sleep(60)"'
            ),
            config.SAY_TIMEOUT_SECONDS_KEY: 0.1,
        },
    )

    outcome = _speech_deadline_outcome(
        lambda: audio.render_speech_audio("hello", repo_root=tmp_path)
    )

    assert outcome.state == "timed-out"
    assert outcome.elapsed_seconds < 1.0
    assert outcome.phase == "serve-speech-external"
    assert outcome.input_label == "characters=5"


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
