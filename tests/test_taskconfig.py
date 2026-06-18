"""Task backend file helpers."""

from __future__ import annotations

from spice.tasks import config


def test_atomic_write_text_keeps_matching_file(tmp_path):
    path = tmp_path / "taskrc"
    config._atomic_write_text(path, "same\n")
    before = path.stat()

    config._atomic_write_text(path, "same\n")
    after = path.stat()

    assert (after.st_ino, after.st_mtime_ns, after.st_size) == (
        before.st_ino,
        before.st_mtime_ns,
        before.st_size,
    )


def test_ensure_task_event_file_preserves_existing_event(tmp_path):
    config.mark_task_backend_changed("unit", root=tmp_path)
    event_path = config.task_event_path(tmp_path)
    event_text = event_path.read_text(encoding="utf-8")

    ensured = config.ensure_task_event_file(tmp_path)

    assert ensured == event_path
    assert ensured.read_text(encoding="utf-8") == event_text
