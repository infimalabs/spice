//! A lane's provider glyph, optionally badged for a fast-mode launch, or
//! omp's pi when the model has no recognized provider.

use wire::CanonicalUuid;
use yew::prelude::*;

use crate::watchband::{self, Emblem, Provider, Standing};

/// What hovering a lane's provider emblem names: its standing title, plus
/// Fast mode when the lane launched in it under a recognized provider.
pub(in crate::browser) fn provider_title(standing: &Standing) -> String {
    let mut title = watchband::standing_title(standing);
    if fast_mode(standing)
        && matches!(
            watchband::emblem(standing.model.as_deref()),
            Emblem::Provider(_)
        )
    {
        title.push_str("; Fast mode");
    }
    title
}

/// The emblem for `standing` as drawn on `surface`, titled and labelled with
/// its [`provider_title`].
pub(in crate::browser) fn render_provider_emblem(
    standing: &Standing,
    surface: &'static str,
) -> Html {
    render(standing, surface, Some(provider_title(standing)))
}

/// The same emblem without its own title or label, for a control that
/// carries [`provider_title`] itself.
pub(in crate::browser) fn render_provider_mark(standing: &Standing, surface: &'static str) -> Html {
    render(standing, surface, None)
}

/// Gradient ids are named by surface and lane: the mosaic stays mounted while
/// hidden, so a lane drawn on two surfaces would otherwise mint one id twice
/// and `url(#id)` would resolve to the hidden copy.
fn render(standing: &Standing, surface: &'static str, title: Option<String>) -> Html {
    let (mark, provider) = match watchband::emblem(standing.model.as_deref()) {
        Emblem::Provider(provider) => {
            let badge = if fast_mode(standing) {
                render_fast_badge()
            } else {
                Html::default()
            };
            (
                render_vendor_glyph(provider_glyph_path(provider), badge),
                Some(provider),
            )
        }
        Emblem::Pi => (render_omp_mark(surface, standing.lane), None),
    };
    let provider_class =
        provider.map(|provider| format!("app-provider-emblem--{}", provider.key()));
    let labelled = title.is_some();
    html! {
        <span
            class={classes!("app-provider-emblem", provider_class)}
            data-provider={provider.map(|provider| provider.key().to_owned())}
            title={title.clone()}
            aria-label={title}
            role={labelled.then_some("img")}
            aria-hidden={(!labelled).then_some("true")}
        >
            {mark}
        </span>
    }
}

const fn fast_mode(standing: &Standing) -> bool {
    standing.thread.is_some() && standing.fast_mode
}

/// The vendor's glyph on the 16-unit canvas it publishes, wearing `badge`
/// over its foot corner or nothing.
fn render_vendor_glyph(path: &'static str, badge: Html) -> Html {
    html! {
        <svg viewBox="0 0 16 16" aria-hidden="true" focusable="false">
            <path d={path}></path>
            {badge}
        </svg>
    }
}

/// oh-my-pi's mark filling the emblem on its own.
fn render_omp_mark(surface: &str, lane: CanonicalUuid) -> Html {
    html! {
        <svg viewBox="0 0 64 64" aria-hidden="true" focusable="false">
            {render_omp_pi(surface, lane)}
        </svg>
    }
}

/// The platform flame glyph marking a fast-mode launch.
const FAST_FLAME: &str = "🔥";

/// [`FAST_FLAME`] over the provider glyph's foot corner.
fn render_fast_badge() -> Html {
    html! {
        <text
            class="app-provider-emblem__fast"
            x="7"
            y="15"
            aria-hidden="true"
        >
            {FAST_FLAME}
        </text>
    }
}

/// oh-my-pi's pi as omp.sh draws it on its 64-unit canvas.
const OMP_PI: &str = "M10 14h44v9H43v33h-9V23h-9v22h-9V23H10z";

/// oh-my-pi's pi as omp.sh draws it, under one diagonal gradient on its
/// 64-unit canvas.
///
/// Every standalone pi carries its own gradient, named by its surface and
/// lane, so the fill resolves inside the icon that paints it.
fn render_omp_pi(surface: &str, lane: CanonicalUuid) -> Html {
    let gradient = format!("{surface}-vendor-gradient-{lane}");
    let fill = format!("url(#{gradient})");
    html! {
        <>
            <defs>
                // A literal tag is normalised to lowercase, and SVG knows no
                // `lineargradient`; the dynamic form keeps its case.
                <@{"linearGradient"} id={gradient} x1="0" y1="0" x2="1" y2="1">
                    <stop offset="0" stop-color="oklch(0.7 0.24 340)"></stop>
                    <stop offset=".5" stop-color="oklch(0.62 0.21 295)"></stop>
                    <stop offset="1" stop-color="oklch(0.81 0.14 200)"></stop>
                </@>
            </defs>
            <path fill={fill} d={OMP_PI}></path>
        </>
    }
}

/// The provider's glyph on a 16-unit canvas, as the provider publishes it.
const fn provider_glyph_path(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => {
            "m3.127 10.604 3.135-1.76.053-.153-.053-.085H6.11l-.525-.032-1.791-.048-1.554-.065-1.505-.08-.38-.081L0 7.832l.036-.234.32-.214.455.04 1.009.069 1.513.105 1.097.064 1.626.17h.259l.036-.105-.089-.065-.068-.064-1.566-1.062-1.695-1.121-.887-.646-.48-.327-.243-.306-.104-.67.435-.48.585.04.15.04.593.456 1.267.981 1.654 1.218.242.202.097-.068.012-.049-.109-.181-.9-1.626-.96-1.655-.428-.686-.113-.411a2 2 0 0 1-.068-.484l.496-.674L4.446 0l.662.089.279.242.411.94.666 1.48 1.033 2.014.302.597.162.553.06.17h.105v-.097l.085-1.134.157-1.392.154-1.792.052-.504.25-.605.497-.327.387.186.319.456-.045.294-.19 1.23-.37 1.93-.243 1.29h.142l.161-.16.654-.868 1.097-1.372.484-.545.565-.601.363-.287h.686l.505.751-.226.775-.707.895-.585.759-.839 1.13-.524.904.048.072.125-.012 1.897-.403 1.024-.186 1.223-.21.553.258.06.263-.218.536-1.307.323-1.533.307-2.284.54-.028.02.032.04 1.029.098.44.024h1.077l2.005.15.525.346.315.424-.053.323-.807.411-3.631-.863-.872-.218h-.12v.073l.726.71 1.331 1.202 1.667 1.55.084.383-.214.302-.226-.032-1.464-1.101-.565-.497-1.28-1.077h-.084v.113l.295.432 1.557 2.34.08.718-.112.234-.404.141-.444-.08-.911-1.28-.94-1.44-.759-1.291-.093.053-.448 4.821-.21.246-.484.186-.403-.307-.214-.496.214-.98.258-1.28.21-1.016.19-1.263.112-.42-.008-.028-.092.012-.953 1.307-1.448 1.957-1.146 1.227-.274.109-.477-.247.045-.44.266-.39 1.586-2.018.956-1.25.617-.723-.004-.105h-.036l-4.212 2.736-.75.096-.324-.302.04-.496.154-.162 1.267-.871z"
        }
        Provider::OpenAi => {
            "M14.949 6.547a3.94 3.94 0 0 0-.348-3.273 4.11 4.11 0 0 0-4.4-1.934A4.1 4.1 0 0 0 8.423.2 4.15 4.15 0 0 0 6.305.086a4.1 4.1 0 0 0-1.891.948 4.04 4.04 0 0 0-1.158 1.753 4.1 4.1 0 0 0-1.563.679A4 4 0 0 0 .554 4.72a3.99 3.99 0 0 0 .502 4.731 3.94 3.94 0 0 0 .346 3.274 4.11 4.11 0 0 0 4.402 1.933c.382.425.852.764 1.377.995.526.231 1.095.35 1.67.346 1.78.002 3.358-1.132 3.901-2.804a4.1 4.1 0 0 0 1.563-.68 4 4 0 0 0 1.14-1.253 3.99 3.99 0 0 0-.506-4.716m-6.097 8.406a3.05 3.05 0 0 1-1.945-.694l.096-.054 3.23-1.838a.53.53 0 0 0 .265-.455v-4.49l1.366.778q.02.011.025.035v3.722c-.003 1.653-1.361 2.992-3.037 2.996m-6.53-2.75a2.95 2.95 0 0 1-.36-2.01l.095.057L5.29 12.09a.53.53 0 0 0 .527 0l3.949-2.246v1.555a.05.05 0 0 1-.022.041L6.473 13.3c-1.454.826-3.311.335-4.15-1.098m-.85-6.94A3.02 3.02 0 0 1 3.07 3.949v3.785a.51.51 0 0 0 .262.451l3.93 2.237-1.366.779a.05.05 0 0 1-.048 0L2.585 9.342a2.98 2.98 0 0 1-1.113-4.094zm11.216 2.571L8.747 5.576l1.362-.776a.05.05 0 0 1 .048 0l3.265 1.86a3 3 0 0 1 1.173 1.207 2.96 2.96 0 0 1-.27 3.2 3.05 3.05 0 0 1-1.36.997V8.279a.52.52 0 0 0-.276-.445m1.36-2.015-.097-.057-3.226-1.855a.53.53 0 0 0-.53 0L6.249 6.153V4.598a.04.04 0 0 1 .019-.04L9.533 2.7a3.07 3.07 0 0 1 3.257.139c.474.325.843.778 1.066 1.303.223.526.289 1.103.191 1.664zM5.503 8.575 4.139 7.8a.05.05 0 0 1-.026-.037V4.049c0-.57.166-1.127.476-1.607s.752-.864 1.275-1.105a3.08 3.08 0 0 1 3.234.41l-.096.054-3.23 1.838a.53.53 0 0 0 .265.455zm.742-1.577 1.758-1 1.762 1v2l-1.755 1-1.762-1z"
        }
    }
}
