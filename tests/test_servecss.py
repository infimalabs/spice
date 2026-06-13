"""Static serve CSS contracts."""

from __future__ import annotations

from spice.serve.web import STATIC_ROOT


def test_mobile_composer_slider_override_follows_base_rule():
    composer_css = (STATIC_ROOT / "composer.css").read_text(encoding="utf-8")
    base_start = composer_css.index(".stack-slider input {")
    base_end = composer_css.index("}", base_start)
    base_rule = composer_css[base_start:base_end]
    mobile_start = composer_css.index("@media (max-width: 720px)", base_end)
    mobile_end = composer_css.index(".stack-slider--armed", mobile_start)
    mobile_rule = composer_css[mobile_start:mobile_end]

    assert "width: 96px;" in base_rule
    assert ".stack-slider { padding: 3px 5px; }" in mobile_rule
    assert ".stack-slider input { width: 68px; }" in mobile_rule
