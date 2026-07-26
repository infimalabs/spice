"""Pytest session setup: pin the agent driver so the suite is deterministic.

The driver is normally resolved from ``SPICE_AGENT_DRIVER`` or worktree config,
so a worktree configured to run Claude would otherwise import a Claude
``DRIVER`` and break fixtures that assert the Codex contract. The suite forces
Codex here, before any spice import resolves the singleton; Claude-specific
behavior is exercised through explicit ``select_driver("claude")`` calls.

Pinning once at import is not enough. ``run_serve`` deliberately scrubs the
driver variables out of the real process environment, so any test that exercises
it erases this pin for every test scheduled behind it in that worker -- after
which driver resolution falls back to the worktree's own configuration and a
Claude checkout quietly renders Claude wrappers. The fixture below re-establishes
the pin ahead of each test so it is an invariant every test starts from rather
than an initial condition the first serve test gets to destroy.
"""

import os

import pytest

# Spelled out rather than imported from spice: importing spice resolves the
# driver singleton, and the pin has to land before that happens. The spelling is
# checked against the real constant below, once importing is safe.
AGENT_DRIVER_ENV = "SPICE_AGENT_DRIVER"  # env-policy: allow
PINNED_AGENT_DRIVER = "codex"

os.environ[AGENT_DRIVER_ENV] = PINNED_AGENT_DRIVER  # env-policy: allow

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
from spice.agent.driver import SPICE_AGENT_DRIVER_ENV, all_drivers  # noqa: E402

if AGENT_DRIVER_ENV != SPICE_AGENT_DRIVER_ENV:
    raise RuntimeError(
        "pytest pins the agent driver through "
        f"{AGENT_DRIVER_ENV!r}, but spice reads {SPICE_AGENT_DRIVER_ENV!r}"
    )

THREAD_ID_ENVS = tuple(_driver.thread_id_env for _driver in all_drivers())

for _name in THREAD_ID_ENVS:
    os.environ.pop(_name, None)  # env-policy: allow


@pytest.fixture(autouse=True)
def pinned_agent_environment():
    """Start every test from the pinned agent environment this module sets up."""

    os.environ[AGENT_DRIVER_ENV] = PINNED_AGENT_DRIVER  # env-policy: allow
    for name in THREAD_ID_ENVS:
        os.environ.pop(name, None)  # env-policy: allow
