"""Pytest session setup: pin the agent driver so the suite is deterministic.

The driver is normally resolved from ``SPICE_AGENT_DRIVER`` or worktree config,
so a worktree configured to run Claude would otherwise import a Claude
``DRIVER`` and break fixtures that assert the Codex contract. The suite forces
Codex here, before any spice import resolves the singleton; Claude-specific
behavior is exercised through explicit ``select_driver("claude")`` calls.
"""

import os

os.environ["SPICE_AGENT_DRIVER"] = "codex"  # env-policy: allow
