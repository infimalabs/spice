"""Serve speech backend rendering."""

from __future__ import annotations

import io
import os
import subprocess
import sys
import time
import wave
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

import pytest

from spice.config import edit, layers, values
from spice.cli.parser import build_parser
from spice.configcli import handle_config
from spice.process.groups import ProcessDeadlineExceeded
from spice.serve import audio

ESPEAK_TEST_SAMPLE_RATE = 8000
LONG_MESSAGE_FLOOR_SECONDS = 60.0
# Rate multipliers a listener can pick in the UI, either side of unscaled.
SLOW_RATE_MULTIPLIER = 0.5
FAST_RATE_MULTIPLIER = 2.0
CONFIGURED_WORDS_PER_MINUTE = 200


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
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {
            values.SAY_VOICE_KEY: "Samantha",
            values.SAY_WORDS_PER_MINUTE_KEY: 200,
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
    assert seen["timeout"] == values.DEFAULT_SAY_TIMEOUT_SECONDS


def test_external_speech_backend_uses_configured_command(tmp_path, monkeypatch):
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {
            values.SAY_BACKEND_KEY: "external",
            values.SAY_COMMAND_KEY: "tts-engine --wav",
            values.SAY_CONTENT_TYPE_KEY: "audio/wav",
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


def test_external_speech_rate_reaches_the_command_through_its_named_slot(
    tmp_path, monkeypatch
):
    # Spice cannot know which flag an arbitrary engine spells its rate with, so
    # the command names the spot. `--voice={en}` is there to prove substitution
    # replaces one named token rather than running a format pass over the argv.
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {
            values.SAY_BACKEND_KEY: "external",
            values.SAY_COMMAND_KEY: "tts-engine --voice={en} -s {words_per_minute}",
            values.SAY_CONTENT_TYPE_KEY: "audio/wav",
            values.SAY_WORDS_PER_MINUTE_KEY: CONFIGURED_WORDS_PER_MINUTE,
        },
    )
    seen: list[list[str]] = []

    def fake_run(args, **kwargs):
        seen.append(args)
        return subprocess.CompletedProcess(args, 0, stdout=b"wav-bytes", stderr=b"")

    monkeypatch.setattr(audio, "run_bounded_process_group", fake_run)

    for rate in (
        SLOW_RATE_MULTIPLIER,
        audio.DEFAULT_SAY_RATE_MULTIPLIER,
        FAST_RATE_MULTIPLIER,
    ):
        audio.render_speech_audio("hello", repo_root=tmp_path, rate_multiplier=rate)

    assert seen == [
        ["tts-engine", "--voice={en}", "-s", "100"],
        ["tts-engine", "--voice={en}", "-s", "200"],
        ["tts-engine", "--voice={en}", "-s", "400"],
    ]


def test_external_speech_command_without_a_slot_keeps_its_own_engine_rate(
    tmp_path, monkeypatch
):
    # The documented contract for a command that names no slot: spice writes no
    # rate into it, so the engine's own rate stands whatever the listener picks.
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {
            values.SAY_BACKEND_KEY: "external",
            values.SAY_COMMAND_KEY: "tts-engine --wav",
            values.SAY_CONTENT_TYPE_KEY: "audio/wav",
            values.SAY_WORDS_PER_MINUTE_KEY: CONFIGURED_WORDS_PER_MINUTE,
        },
    )
    seen: list[list[str]] = []

    def fake_run(args, **kwargs):
        seen.append(args)
        return subprocess.CompletedProcess(args, 0, stdout=b"wav-bytes", stderr=b"")

    monkeypatch.setattr(audio, "run_bounded_process_group", fake_run)

    for rate in (SLOW_RATE_MULTIPLIER, FAST_RATE_MULTIPLIER):
        audio.render_speech_audio("hello", repo_root=tmp_path, rate_multiplier=rate)

    assert seen == [["tts-engine", "--wav"], ["tts-engine", "--wav"]]


def test_external_speech_backend_reports_command_failure(tmp_path, monkeypatch):
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {
            values.SAY_BACKEND_KEY: "external",
            values.SAY_COMMAND_KEY: "tts-engine",
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
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {
            values.SAY_BACKEND_KEY: "external",
            values.SAY_COMMAND_KEY: "tts-engine",
            values.SAY_TIMEOUT_SECONDS_KEY: 0.25,
        },
    )
    seen: dict[str, object] = {}

    def fake_run(args, **kwargs):
        seen["timeout"] = kwargs["timeout_seconds"]
        return subprocess.CompletedProcess(args, 0, stdout=b"wav-bytes", stderr=b"")

    monkeypatch.setattr(audio, "run_bounded_process_group", fake_run)

    audio.render_speech_audio("hello", repo_root=tmp_path)

    assert seen["timeout"] == 0.25


def test_infinite_layered_speech_timeout_renders_with_finite_default(
    tmp_path, monkeypatch
):
    (tmp_path / "spice.toml").write_text(
        '[say]\nbackend = "external"\ncommand = "tts-engine"\ntimeout_seconds = inf\n',
        encoding="utf-8",
    )
    seen: dict[str, object] = {}

    def fake_run(args, **kwargs):
        seen["timeout"] = kwargs["timeout_seconds"]
        return subprocess.CompletedProcess(args, 0, stdout=b"wav-bytes", stderr=b"")

    monkeypatch.setattr(audio, "run_bounded_process_group", fake_run)

    rendered = audio.render_speech_audio("hello", repo_root=tmp_path)

    assert {
        "rendered": rendered,
        "timeout": seen["timeout"],
        "finite": seen["timeout"] < float("inf"),
    } == {
        "rendered": audio.SpeechAudio(b"wav-bytes", "audio/wav"),
        "timeout": values.DEFAULT_SAY_TIMEOUT_SECONDS,
        "finite": True,
    }


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
    assert seen["timeout"] == values.DEFAULT_SAY_TIMEOUT_SECONDS
    assert seen["timeout"] > LONG_MESSAGE_FLOOR_SECONDS


def test_stalled_external_speech_releases_worker_with_named_deadline(tmp_path):
    edit.set_scope_section(
        tmp_path,
        layers.WORKTREE_SOURCE,
        values.SAY_KEY,
        {
            values.SAY_BACKEND_KEY: "external",
            values.SAY_COMMAND_KEY: (
                f'{sys.executable} -c "import time; time.sleep(60)"'
            ),
            values.SAY_TIMEOUT_SECONDS_KEY: 0.1,
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
        "with Path('espeak-ng.argv').open('a') as record:\n"
        "    record.write(' '.join(sys.argv[1:]) + '\\n')\n"
        "with wave.open(sys.stdout.buffer, 'wb') as output:\n"
        "    output.setnchannels(1)\n"
        "    output.setsampwidth(2)\n"
        f"    output.setframerate({ESPEAK_TEST_SAMPLE_RATE})\n"
        "    output.writeframes(b'\\x00\\x00')\n",
        encoding="utf-8",
    )
    executable.chmod(0o755)
    monkeypatch.setenv("PATH", str(executable_dir), prepend=os.pathsep)
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
                "espeak-ng --stdout -s {words_per_minute}",
                "--content-type",
                "audio/wav",
            ]
        )
    )
    for rate in (SLOW_RATE_MULTIPLIER, audio.DEFAULT_SAY_RATE_MULTIPLIER):
        rendered = audio.render_speech_audio(
            "see [Linux docs](https://example.test)/today",
            repo_root=tmp_path,
            rate_multiplier=rate,
        )

    assert result == 0
    assert capsys.readouterr().out == (
        "say backend=external command=espeak-ng --stdout -s {words_per_minute} "
        "content_type=audio/wav\n"
    )
    assert (tmp_path / "espeak-ng.stdin").read_bytes() == b"see Linux docs today"
    # The listener's rate reaches the engine through the slot the documented
    # recipe names, scaled from the default words-per-minute base.
    assert (tmp_path / "espeak-ng.argv").read_text(encoding="utf-8") == (
        "--stdout -s 88\n--stdout -s 175\n"
    )
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
        "spice config say --backend external --command "
        '"espeak-ng --stdout -s {words_per_minute}" --content-type audio/wav'
        in reference
    )
    assert "returned on stdout as `audio/wav`" in reference
    # Read the rate contract as prose rather than as laid-out lines, so
    # rewrapping the paragraph cannot fail a test about what it promises.
    prose = " ".join(reference.split())
    assert (
        "every `{words_per_minute}` token in the command is replaced with "
        "`say.words_per_minute` scaled by the rate the listener picked in the UI"
        in prose
    )
    assert "Substitution replaces that one token and nothing else" in prose
    assert (
        "A command naming no slot renders at whatever rate its own engine "
        "defaults to" in prose
    )
