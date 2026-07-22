"""Pytest session setup: pin the agent driver so the suite is deterministic.

The driver is normally resolved from ``SPICE_AGENT_DRIVER`` or worktree config,
so a worktree configured to run Claude would otherwise import a Claude
``DRIVER`` and break fixtures that assert the Codex contract. The suite forces
Codex here, before any spice import resolves the singleton; Claude-specific
behavior is exercised through explicit ``select_driver("claude")`` calls.
"""

import os

os.environ["SPICE_AGENT_DRIVER"] = "codex"  # env-policy: allow

# The suite runs inside an agent shell that injects a git shadow
# (GIT_CONFIG_SYSTEM + GIT_CONFIG_KEY/VALUE/COUNT pairs, and possibly GIT_DIR).
# Scrub every GIT_* var so tests build and read their own repos hermetically and
# never inherit a lane's self-tracking shadow.
for _name in [_n for _n in os.environ if _n.startswith("GIT_")]:  # env-policy: allow
    del os.environ[_name]  # env-policy: allow

# That agent shell also exports its own driver thread id, and identity resolution
# scans every driver rather than only the pinned one -- so a suite pinned to Codex
# still sees a live Claude session id and adopts it. Left in place it makes the
# running lane the ambient actor: tw.current_actor() prefers it over the sentinel,
# and claim_meta() stamps it onto claims tests believe are unowned. Scrub from the
# driver table so the set stays correct as drivers are added.
from spice.agent.driver import all_drivers  # noqa: E402

for _driver in all_drivers():
    os.environ.pop(_driver.thread_id_env, None)  # env-policy: allow
