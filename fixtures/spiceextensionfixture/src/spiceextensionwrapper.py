from __future__ import annotations


def toy_wrapper_spec() -> dict[str, list[str]]:
    return {"argv": ["toy-wrapper-bin", "--from-entry-point"]}


def shadow_spice_dev_wrapper_spec() -> dict[str, list[str]]:
    return {"argv": ["shadow-spice-dev", "--from-entry-point"]}
