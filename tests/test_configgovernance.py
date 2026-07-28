"""Class-level constitution gates for configuration safety."""

from __future__ import annotations

from pathlib import Path

import pytest

from spice.config import layers, trust
from spice.errors import SpiceError
from spice.studies import configgovernance


def test_key_validity_gate_refuses_an_unknown_key_in_the_current_source(tmp_path):
    config = tmp_path / "spice.toml"
    config.write_text('[serve]\nbrnad = "Typo"\n', encoding="utf-8")

    with pytest.raises(SpiceError) as exc_info:
        configgovernance.run_config_key_validity_gate(tmp_path)

    message = str(exc_info.value)
    assert "unknown configuration key serve.brnad" in message
    assert "did you mean serve.brand?" in message
    assert f"source=repository path={config}" in message


def test_key_validity_gate_checks_the_candidate_packaged_configuration(tmp_path):
    packaged = tmp_path / "spice" / "spice.toml"
    packaged.parent.mkdir()
    packaged.write_text('[serve]\nbrnad = "Typo"\n', encoding="utf-8")

    with pytest.raises(SpiceError) as exc_info:
        configgovernance.run_config_key_validity_gate(tmp_path)

    message = str(exc_info.value)
    assert "unknown configuration key serve.brnad" in message
    assert f"source=system path={packaged}" in message


def test_false_disable_gate_covers_every_live_shared_consumer(tmp_path):
    assert configgovernance.run_false_disable_gate(tmp_path) is None


def test_false_disable_gate_refuses_a_declared_registry_without_a_consumer(
    tmp_path, monkeypatch
):
    monkeypatch.setattr(
        layers,
        "FALSE_DISABLE_REGISTRY_PATHS",
        (*layers.FALSE_DISABLE_REGISTRY_PATHS, ("tasks", "flows")),
    )

    with pytest.raises(SpiceError, match="missing=tasks.flows"):
        configgovernance.run_false_disable_gate(tmp_path)


def test_tracked_file_trust_gate_covers_every_live_approval_guard(tmp_path):
    assert configgovernance.run_tracked_file_trust_gate(tmp_path) is None


def test_tracked_file_trust_gate_refuses_an_executable_root_without_a_guard(
    tmp_path, monkeypatch
):
    unguarded = ("agent", "playwright_mcp", "command")
    monkeypatch.setattr(
        trust,
        "EXECUTABLE_REPOSITORY_CONFIG_PATHS",
        (*trust.EXECUTABLE_REPOSITORY_CONFIG_PATHS, unguarded),
    )

    with pytest.raises(
        SpiceError,
        match=r"missing=agent\.playwright_mcp\.command",
    ):
        configgovernance.run_tracked_file_trust_gate(tmp_path)


def test_approval_guard_refuses_a_path_absent_from_the_digest_inventory(tmp_path):
    with pytest.raises(SpiceError, match="absent from EXECUTABLE"):
        trust.require_repository_config_approval(
            Path(tmp_path),
            ("agent", "playwright_mcp", "command"),
            command="unapproved-tool",
        )
