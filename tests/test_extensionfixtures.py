from __future__ import annotations

from tests.test_extensionhelpers import (
    build_fixture_wheel,
    entry_point_map,
    single_distribution_from_wheel,
)


def test_extension_fixture_wheel_exposes_real_entry_points(tmp_path, monkeypatch):
    wheel = build_fixture_wheel(tmp_path)

    distribution = single_distribution_from_wheel(wheel)
    entry_points = entry_point_map(distribution)

    assert distribution.metadata["Name"] == "spice-extension-fixture"
    assert distribution.metadata["Version"] == "0.1.0"
    assert sorted(entry_points) == [
        "spice.drivers",
        "spice.studies",
        "spice.wrappers",
    ]
    assert sorted(entry_points["spice.drivers"]) == ["codex", "toy"]
    assert sorted(entry_points["spice.studies"]) == ["file-loc", "toy-study"]
    assert sorted(entry_points["spice.wrappers"]) == ["spice-dev", "toy-wrapper"]

    monkeypatch.syspath_prepend(str(wheel))
    toy_driver = entry_points["spice.drivers"]["toy"].load()
    shadow_driver = entry_points["spice.drivers"]["codex"].load()
    toy_study = entry_points["spice.studies"]["toy-study"].load()
    shadow_study = entry_points["spice.studies"]["file-loc"].load()
    toy_wrapper = entry_points["spice.wrappers"]["toy-wrapper"].load()
    shadow_wrapper = entry_points["spice.wrappers"]["spice-dev"].load()

    assert toy_driver.name == "toy"
    assert toy_driver.build_exec_command(
        repo_root=tmp_path,
        prompt="hello",
        thread_id="thread-1",
        model="toy-large",
    ) == ["toy-agent", "exec", "--thread", "thread-1", "--model", "toy-large", "hello"]
    assert shadow_driver.name == "codex"
    assert toy_study(["a.py"]) == {"study": "toy", "paths": ["a.py"]}
    assert shadow_study(["b.py"]) == {"study": "file-loc", "paths": ["b.py"]}
    assert toy_wrapper() == {"argv": ["toy-wrapper-bin", "--from-entry-point"]}
    assert shadow_wrapper() == {"argv": ["shadow-spice-dev", "--from-entry-point"]}
