from pathlib import Path

import pytest

from spice.config import layers
from spice.errors import SpiceError
from spice.scopes import (
    MAXIM_SCOPES,
    NON_SELECTOR_CONCEPTS,
    POLICY_RULE_SCOPES,
    PRE_COMMIT_STEP_SCOPES,
    SCOPE_AXIS_CONSUMERS,
    STUDY_PROVIDER_SCOPES,
    WRAPPER_ROUTE_SCOPES,
    WRAPPER_SCOPES,
    ScopeAxis,
    ScopeContext,
    ScopeSelector,
)


@pytest.mark.parametrize(
    ("consumer", "raw", "context", "expected"),
    (
        (
            STUDY_PROVIDER_SCOPES,
            {"paths": ["./spice\\agent", "spice/agent"]},
            ScopeContext(path="spice/agent/driver.py"),
            ScopeSelector(paths=("spice/agent",)),
        ),
        (
            WRAPPER_SCOPES,
            {"drivers": ["CODEX", "codex"]},
            ScopeContext(driver="Codex"),
            ScopeSelector(drivers=("codex",)),
        ),
        (
            PRE_COMMIT_STEP_SCOPES,
            {"phases": ["pre_commit_success"]},
            ScopeContext(phase="pre-commit-success"),
            ScopeSelector(phases=("pre-commit-success",)),
        ),
        (
            PRE_COMMIT_STEP_SCOPES,
            {"models": [" GPT-5.5 ", "gpt-5.5"]},
            ScopeContext(model="GPT-5.5"),
            ScopeSelector(models=("gpt-5.5",)),
        ),
        (
            POLICY_RULE_SCOPES,
            {"extensions": [".PY", ".py"]},
            ScopeContext(path="spice/scopes.py"),
            ScopeSelector(extensions=(".py",)),
        ),
    ),
)
def test_each_admitted_axis_normalizes_and_matches(consumer, raw, context, expected):
    selector = consumer.parse(raw)

    assert selector == expected
    assert selector.matches(context) is True


def test_consumer_normalizer_is_the_same_contract_used_by_the_parser():
    direct = WRAPPER_SCOPES.normalize(
        ScopeSelector(drivers=("CODEX", "codex", "claude"))
    )
    parsed = WRAPPER_SCOPES.parse({"drivers": ["claude", "codex"]})

    assert direct == parsed == ScopeSelector(drivers=("claude", "codex"))


def test_path_axis_has_identical_semantics_across_all_path_consumers():
    consumers = (
        POLICY_RULE_SCOPES,
        STUDY_PROVIDER_SCOPES,
        PRE_COMMIT_STEP_SCOPES,
    )
    context = ScopeContext(path="spice/agent/driver.py")
    selectors = tuple(
        consumer.parse({"paths": ["./tests", "spice\\agent"]}) for consumer in consumers
    )
    evaluations = tuple(selector.evaluate(context) for selector in selectors)

    assert selectors == (ScopeSelector(paths=("spice/agent", "tests")),) * 3
    assert evaluations == (evaluations[0],) * 3


def test_driver_axis_has_identical_semantics_across_all_driver_consumers():
    consumers = (
        PRE_COMMIT_STEP_SCOPES,
        WRAPPER_SCOPES,
        WRAPPER_ROUTE_SCOPES,
        MAXIM_SCOPES,
    )
    context = ScopeContext(driver="codex")
    selectors = tuple(
        consumer.parse({"drivers": ["codex", "claude"]}) for consumer in consumers
    )
    evaluations = tuple(selector.evaluate(context) for selector in selectors)

    assert selectors == (ScopeSelector(drivers=("claude", "codex")),) * 4
    assert evaluations == (evaluations[0],) * 4


def test_scope_axes_compose_or_within_and_and_across_with_one_explanation():
    selector = POLICY_RULE_SCOPES.parse(
        {
            "paths": ["docs", "src/**"],
            "extensions": [".md", ".py"],
        }
    )

    matched = selector.evaluate(ScopeContext(path="src/pkg/app.py"))
    extension_miss = selector.evaluate(ScopeContext(path="src/pkg/app.txt"))

    assert matched.matched is True
    assert matched.explanation == (
        "scopes match=true: paths any-of [docs, src/**] "
        "actual=src/pkg/app.py match=true; extensions any-of [.md, .py] "
        "actual=.py match=true"
    )
    assert extension_miss.matched is False
    assert extension_miss.explanation == (
        "scopes match=false: paths any-of [docs, src/**] "
        "actual=src/pkg/app.txt match=true; extensions any-of [.md, .py] "
        "actual=.txt match=false"
    )


def test_pre_commit_paths_drivers_models_and_phases_compose_deterministically():
    selector = PRE_COMMIT_STEP_SCOPES.parse(
        {
            "paths": ["docs/**", "spice/**"],
            "drivers": ["codex", "claude"],
            "models": ["GPT-5.5", "gpt-5.4"],
            "phases": ["pre_commit", "pre_commit_success"],
        }
    )

    evaluation = selector.evaluate(
        ScopeContext(
            path="spice/scopes.py",
            driver="CODEX",
            model="gpt-5.5",
            phase="pre-commit-success",
        )
    )

    assert evaluation.matched is True
    assert evaluation.explanation == (
        "scopes match=true: paths any-of [docs/**, spice/**] "
        "actual=spice/scopes.py match=true; drivers any-of [claude, codex] "
        "actual=codex match=true; models any-of [gpt-5.4, gpt-5.5] "
        "actual=gpt-5.5 match=true; phases any-of "
        "[pre-commit, pre-commit-success] actual=pre-commit-success match=true"
    )


def test_absent_axes_are_unconstrained():
    selector = MAXIM_SCOPES.parse({})
    context = ScopeContext(
        path="any/file.txt",
        driver="codex",
        model="gpt-5.5",
        phase="pre-commit",
        extension=".txt",
    )

    evaluation = selector.evaluate(context)

    assert evaluation.matched is True
    assert evaluation.explanation == "scopes match=true: unconstrained"
    assert evaluation.specificity == selector.specificity(context)
    assert evaluation.specificity.constrained_axes == 0


def test_specificity_prefers_more_axes_then_the_canonical_path_rule():
    context = ScopeContext(path="src/pkg/app.py")
    broad = POLICY_RULE_SCOPES.parse({"paths": ["src/**"]})
    exact = POLICY_RULE_SCOPES.parse({"paths": ["src/pkg/app.py"]})
    exact_with_alternative = POLICY_RULE_SCOPES.parse(
        {"paths": ["src/pkg/app.py", "tests"]}
    )
    exact_extension = POLICY_RULE_SCOPES.parse(
        {"paths": ["src/pkg/app.py"], "extensions": [".py"]}
    )

    ordered = sorted(
        (broad, exact_extension, exact, exact_with_alternative),
        key=lambda selector: selector.specificity(context),
    )

    assert ordered == [broad, exact_with_alternative, exact, exact_extension]
    assert exact_extension.specificity(context).normalized_values == (
        ("src/pkg/app.py",),
        (".py",),
        (),
        (),
        (),
    )


def test_specificity_prefers_fewer_or_alternatives_on_value_axes():
    context = ScopeContext(driver="codex")
    broad = WRAPPER_SCOPES.parse({"drivers": ["claude", "codex"]})
    narrow = WRAPPER_SCOPES.parse({"drivers": ["codex"]})

    assert sorted(
        (narrow, broad), key=lambda selector: selector.specificity(context)
    ) == [broad, narrow]


def test_consumer_inventory_derives_every_admitted_axis_from_live_users():
    assert SCOPE_AXIS_CONSUMERS == {
        ScopeAxis.PATHS: (
            "policy-rule",
            "study-provider",
            "pre-commit-step",
        ),
        ScopeAxis.EXTENSIONS: ("policy-rule",),
        ScopeAxis.DRIVERS: (
            "pre-commit-step",
            "wrapper",
            "wrapper-route",
            "maxim",
        ),
        ScopeAxis.MODELS: ("pre-commit-step",),
        ScopeAxis.PHASES: ("pre-commit-step",),
    }
    assert NON_SELECTOR_CONCEPTS == {
        "commands": "wrapper command heads and flags are routing payload",
        "languages": "language families are classification datasets",
        "roles": "test and generated roles are classification datasets",
        "task-phases": "task phases are live allocator routing state",
        "configuration-layers": (
            "system, pyproject, repository, and worktree are precedence metadata"
        ),
    }
    assert WRAPPER_ROUTE_SCOPES.supported_axes == WRAPPER_SCOPES.supported_axes


@pytest.mark.parametrize("concept", tuple(NON_SELECTOR_CONCEPTS))
def test_inventoried_payload_and_dataset_concepts_use_canonical_rejection(concept):
    with pytest.raises(SpiceError) as exc_info:
        PRE_COMMIT_STEP_SCOPES.parse({concept: ["fixture"]})

    assert str(exc_info.value) == (
        "scopes for consumer 'pre-commit-step' "
        f"(supported axes: paths, drivers, models, phases): unsupported axes: {concept}"
    )


def test_supported_set_is_named_for_known_but_unsupported_axis():
    with pytest.raises(SpiceError) as exc_info:
        MAXIM_SCOPES.parse({"paths": ["spice"]})

    assert str(exc_info.value) == (
        "scopes for consumer 'maxim' (supported axes: drivers): unsupported axes: paths"
    )


@pytest.mark.parametrize(
    ("raw", "detail"),
    (
        ([], "must be an inline table"),
        (
            {"paths": []},
            "axis 'paths' must be a non-empty list of non-empty strings",
        ),
        (
            {"paths": ["../outside"]},
            "axis 'paths' contains a non-repository-relative selector: '../outside'",
        ),
    ),
)
def test_malformed_selector_uses_the_same_consumer_diagnostic(raw, detail):
    with pytest.raises(SpiceError) as exc_info:
        STUDY_PROVIDER_SCOPES.parse(raw)

    assert str(exc_info.value) == (
        "scopes for consumer 'study-provider' (supported axes: paths): " + detail
    )


def test_scopes_inline_leaf_replaces_completely_across_four_layers(
    tmp_path, monkeypatch
):
    system_root = tmp_path / "runtime"
    system_root.mkdir()
    monkeypatch.setattr(layers.paths, "runtime_spice_source", lambda: system_root)
    _write(
        system_root / "spice.toml",
        "[policy.pre_commit_builtins.formatters]\n"
        'scopes = { paths = ["system"], drivers = ["codex"] }\n',
    )
    _write(
        tmp_path / "pyproject.toml",
        "[tool.spice.policy.pre_commit_builtins.formatters]\n"
        'scopes = { drivers = ["claude"], phases = ["pre-commit"] }\n',
    )
    _write(
        tmp_path / "spice.toml",
        "[policy.pre_commit_builtins.formatters]\n"
        'scopes = { paths = ["repository"], models = ["gpt"] }\n',
    )
    worktree = tmp_path / ".spice" / "config" / "spice.toml"
    _write(
        worktree,
        "[policy.pre_commit_builtins.formatters]\n"
        'scopes = { models = ["gpt-worktree"] }\n',
    )

    loaded = layers.load_config(tmp_path)
    path = "policy.pre_commit_builtins.formatters.scopes"

    scopes = loaded.effective["policy"]["pre_commit_builtins"]["formatters"]["scopes"]
    assert scopes == {"models": ("gpt-worktree",)}
    assert loaded.source_for(path) == loaded.layer(layers.WORKTREE_SOURCE)
    assert loaded.source_for(f"{path}.models") == loaded.layer(layers.WORKTREE_SOURCE)
    assert loaded.source_for(f"{path}.paths") is None


def test_rendered_driver_explanation_uses_normalized_actual_and_alternatives():
    selector = WRAPPER_ROUTE_SCOPES.parse({"drivers": ["codex", "claude"]})

    assert selector.explain(ScopeContext(driver="CODEX")) == (
        "scopes match=true: drivers any-of [claude, codex] actual=codex match=true"
    )


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
