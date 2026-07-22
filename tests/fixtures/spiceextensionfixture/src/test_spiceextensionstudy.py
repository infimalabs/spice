from __future__ import annotations


def run_toy_study(paths: list[str] | None = None) -> dict[str, object]:
    return {"study": "toy", "paths": list(paths or [])}


def shadow_file_loc_study(paths: list[str] | None = None) -> dict[str, object]:
    return {"study": "file-loc", "paths": list(paths or [])}
