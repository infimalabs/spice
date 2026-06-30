from __future__ import annotations

import pytest

from spice.errors import SpiceError
from spice.hooks import commitmsg


def test_commit_msg_rejects_co_authored_by_trailer(tmp_path):
    message = (
        "Block delegated commit authorship\n"
        "\n"
        "The harness owns the visible commit author contract.\n"
        "\n"
        "Co-Authored-By: Agent <agent@example.test>\n"
    )
    path = tmp_path / "COMMIT_EDITMSG"
    path.write_text(message, encoding="utf-8")

    with pytest.raises(SpiceError) as exc_info:
        commitmsg.handle_commit_msg(str(path), tmp_path)

    error = str(exc_info.value)
    assert "blocked trailer co-authored-by" in error
    assert "blocked trailers: co-authored-by" in error


def test_validate_blocks_only_configured_trailers():
    # The block is configuration-driven, not baked in: a configured trailer is
    # rejected, and with no blocked set even Co-Authored-By passes.
    with pytest.raises(SpiceError) as exc_info:
        commitmsg.validate_commit_message_text(
            "Subject line\n\nX-Internal: secret\n",
            blocked_trailers=frozenset({"x-internal"}),
        )
    assert "blocked trailer x-internal" in str(exc_info.value)

    commitmsg.validate_commit_message_text(
        "Subject line\n\nCo-Authored-By: A <a@example.test>\n",
        blocked_trailers=None,
    )


def test_commit_msg_rejects_wip_subject_and_accepts_real_subject(tmp_path):
    placeholder = tmp_path / "PLACEHOLDER_COMMIT_EDITMSG"
    placeholder.write_text("wip\n", encoding="utf-8")

    with pytest.raises(SpiceError) as exc_info:
        commitmsg.handle_commit_msg(str(placeholder), tmp_path)

    error = str(exc_info.value)
    assert "subject 'wip' is a placeholder" in error
    assert "write a real subject describing the change" in error

    real = tmp_path / "REAL_COMMIT_EDITMSG"
    real.write_text("Block placeholder commit subjects\n", encoding="utf-8")

    assert commitmsg.handle_commit_msg(str(real), tmp_path) == 0


def test_commit_msg_uses_configured_wrap_limit(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy.limits]\ncommit_message_wrap = 20\n",
        encoding="utf-8",
    )
    path = tmp_path / "COMMIT_EDITMSG"
    path.write_text(
        "Hook policy wiring\n\nalpha bravo charlie delta echo foxtrot\n",
        encoding="utf-8",
    )

    assert commitmsg.handle_commit_msg(str(path), tmp_path) == 0

    assert path.read_text(encoding="utf-8").splitlines() == [
        "Hook policy wiring",
        "",
        "alpha bravo charlie",
        "delta echo foxtrot",
    ]


def test_commit_msg_uses_configured_allowed_trailers(tmp_path):
    (tmp_path / "pyproject.toml").write_text(
        '[tool.spice.policy.commit_message]\nallowed_trailers = ["Task"]\n',
        encoding="utf-8",
    )
    allowed = tmp_path / "ALLOWED_COMMIT_EDITMSG"
    allowed.write_text(
        "Record task metadata\n\nTask: HOOKS-1\n",
        encoding="utf-8",
    )
    disallowed = tmp_path / "DISALLOWED_COMMIT_EDITMSG"
    disallowed.write_text(
        "Record review metadata\n\nReviewed-By: Agent <agent@example.test>\n",
        encoding="utf-8",
    )

    assert commitmsg.handle_commit_msg(str(allowed), tmp_path) == 0
    with pytest.raises(SpiceError) as exc_info:
        commitmsg.handle_commit_msg(str(disallowed), tmp_path)

    error = str(exc_info.value)
    assert "disallowed trailer reviewed-by" in error
    assert "allowed trailers: task" in error


def test_commit_msg_default_allows_non_forbidden_trailers(tmp_path):
    path = tmp_path / "COMMIT_EDITMSG"
    path.write_text(
        "Record review metadata\n\nReviewed-By: Agent <agent@example.test>\n",
        encoding="utf-8",
    )

    assert commitmsg.handle_commit_msg(str(path), tmp_path) == 0
