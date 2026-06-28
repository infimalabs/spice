from pathlib import Path

from spice.studies import walk


def test_configured_test_roots_default_to_existing_tests_directory(tmp_path):
    tests = tmp_path / "tests"
    tests.mkdir()

    assert walk.test_path_patterns(tmp_path) == ("tests",)
    assert walk.configured_test_roots(tmp_path) == [tests]
    assert walk.is_test_path(Path("tests/test_sample.py"), tmp_path)


def test_configured_test_roots_empty_when_no_default_tests_exist(tmp_path):
    assert walk.configured_test_roots(tmp_path) == []
    assert not walk.is_test_path(Path("src/app.py"), tmp_path)


def test_policy_test_paths_override_pytest_and_default_with_multi_root_glob(tmp_path):
    (tmp_path / "tests").mkdir()
    (tmp_path / "pytest_tests").mkdir()
    (tmp_path / "specs").mkdir()
    unity_tests = tmp_path / "Assets" / "Game" / "Tests"
    unity_tests.mkdir(parents=True)
    (tmp_path / "pyproject.toml").write_text(
        "[tool.spice.policy]\n"
        'test_paths = ["specs", "Assets/**/Tests"]\n'
        "[tool.pytest.ini_options]\n"
        'testpaths = ["pytest_tests"]\n',
        encoding="utf-8",
    )

    assert walk.test_path_patterns(tmp_path) == ("specs", "Assets/**/Tests")
    assert walk.configured_test_roots(tmp_path) == [tmp_path / "specs", unity_tests]
    assert walk.is_test_path(Path("specs/test_contract.py"), tmp_path)
    assert walk.is_test_path(Path("Assets/Game/Tests/TestThing.cs"), tmp_path)
    assert not walk.is_test_path(Path("tests/test_default.py"), tmp_path)
    assert not walk.is_test_path(Path("pytest_tests/test_pytest.py"), tmp_path)


def test_pytest_testpaths_list_drives_test_roots_when_policy_is_absent(tmp_path):
    (tmp_path / "unit").mkdir()
    (tmp_path / "integration").mkdir()
    (tmp_path / "pyproject.toml").write_text(
        '[tool.pytest.ini_options]\ntestpaths = ["unit", "integration"]\n',
        encoding="utf-8",
    )

    assert walk.test_path_patterns(tmp_path) == ("unit", "integration")
    assert walk.configured_test_roots(tmp_path) == [
        tmp_path / "unit",
        tmp_path / "integration",
    ]


def test_pytest_testpaths_string_is_split_like_ini_values(tmp_path):
    (tmp_path / "unit").mkdir()
    (tmp_path / "integration").mkdir()
    (tmp_path / "pyproject.toml").write_text(
        '[tool.pytest.ini_options]\ntestpaths = "unit integration"\n',
        encoding="utf-8",
    )

    assert walk.test_path_patterns(tmp_path) == ("unit", "integration")
    assert walk.configured_test_roots(tmp_path) == [
        tmp_path / "unit",
        tmp_path / "integration",
    ]


def test_is_test_path_accepts_absolute_paths_inside_repo(tmp_path):
    tests = tmp_path / "tests"
    tests.mkdir()
    path = tests / "test_sample.py"

    assert walk.is_test_path(path, tmp_path)
    assert not walk.is_test_path(tmp_path.parent / "tests" / "test_sample.py", tmp_path)
