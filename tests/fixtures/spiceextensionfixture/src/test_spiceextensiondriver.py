from __future__ import annotations

import re
from pathlib import Path

from spice.agent.driver import AgentDriver


class ToyAgentDriver(AgentDriver):
    def home(self) -> Path:
        return Path.home() / ".spice-extension-fixture"

    def thread_transcript_path(
        self, thread_id: str, *, must_exist: bool = True
    ) -> Path:
        del must_exist
        return self.home() / f"{thread_id}.jsonl"

    def owns_transcript(self, path: Path) -> bool:
        return ".spice-extension-fixture" in path.parts

    def build_exec_command(
        self,
        *,
        repo_root: Path,
        prompt: str,
        thread_id: str = "",
        model: str = "",
        reasoning_effort: str = "",
        personality: str = "",
        service_tier: str = "",
        binary: str = "",
        fast_mode: bool = False,
    ) -> list[str]:
        del repo_root, reasoning_effort, personality, service_tier, fast_mode
        command = [binary or self.binary(), "exec"]
        if thread_id:
            command.extend(["--thread", thread_id])
        if model:
            command.extend(["--model", model])
        command.append(prompt)
        return command


def _driver(name: str) -> ToyAgentDriver:
    return ToyAgentDriver(
        name=name,
        default_bin="toy-agent",
        bin_env="FAKEENV_THIRD_BIN",
        thread_id_env="FAKEENV_THIRD_THREAD_ID",
        default_model="toy-model",
        default_reasoning_effort="low",
        default_service_tier="",
        stdout_assistant_marker="[toy-assistant]",
        stdout_section_markers=frozenset({"[toy-done]"}),
        stdout_compaction_marker="[toy-compacted]",
        session_id_pattern=re.compile(r"session=(?P<id>[A-Za-z0-9_-]+)"),
        default_context_window=128000,
    )


TOY_DRIVER = _driver("toy")
SHADOW_CODEX_DRIVER = _driver("codex")
