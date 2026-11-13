//! The app's one content-agnostic right-edge sidebar surface.

use super::*;

/// Standard side-view widths. Content takes the one its layout needs.
#[derive(Clone, Copy)]
pub(super) enum Width {
    Small,
    Large,
}

impl Width {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Large => "large",
        }
    }
}

/// The base a whole sheet, header and body, wears instead of the page's.
#[derive(Clone, Copy)]
pub(super) enum Base {
    /// The operator's achromatic base.
    Neutral,
    /// One palette index.
    Accent(usize),
}

pub(super) struct Content {
    pub label: AttrValue,
    /// What stands beside the kicker, which names the dialog.
    pub heading: Html,
    pub width: Width,
    /// `None` keeps the page's base.
    pub base: Option<Base>,
    pub body: Html,
    /// The launcher's twin, standing over the launcher's own place and closing
    /// the sidebar in place of the Close button. It sits beside the fading
    /// surface rather than inside it, so the trigger never changes while the
    /// view appears. It carries the sidebar's close ref and close callback.
    pub anchor: Option<Html>,
}

pub(super) struct SidebarRender<'a> {
    pub content: Content,
    pub presented: bool,
    pub closing: bool,
    pub close_ref: &'a NodeRef,
    pub sheet_ref: &'a NodeRef,
    pub close: Callback<()>,
}

pub(super) fn render(render: SidebarRender<'_>) -> Html {
    let close_click = render.close.reform(|_: MouseEvent| ());
    let state = if render.closing {
        "closing"
    } else if render.presented {
        "open"
    } else {
        "opening"
    };
    let close_label = format!("Close {}", render.content.label.to_lowercase());
    let anchored = render.content.anchor.is_some();
    html! {
        <div
            class="app-sidebar-layer"
            data-sidebar-state={state}
            data-sidebar-width={render.content.width.as_str()}
            data-sidebar-anchored={anchored.then_some("true")}
            inert={render.closing.then_some("true")}
        >
            <button
                type="button"
                class="app-sidebar-backdrop app-overlay-veil"
                aria-label={close_label.clone()}
                onclick={close_click.clone()}
            ></button>
            <aside
                class="app-sidebar"
                role="dialog"
                aria-modal="true"
                aria-labelledby="app-sidebar-title"
                data-base-flavor={render.content.base.map(|base| match base {
                    Base::Neutral => "neutral",
                    Base::Accent(_) => "sheet",
                })}
                data-base-accent={render.content.base.and_then(|base| match base {
                    Base::Neutral => None,
                    Base::Accent(accent) => Some(accent.to_string()),
                })}
            >
                {render.content.anchor.unwrap_or_default()}
                <div ref={render.sheet_ref.clone()} class="app-sidebar-surface app-overlay-sheet">
                    <div class="app-sidebar-frame" data-sidebar-content={render.content.label.to_lowercase()}>
                        <header class="app-sidebar-header">
                            <div>
                                <p id="app-sidebar-title" class="app-sidebar-kicker">{render.content.label}</p>
                                {render.content.heading}
                            </div>
                            if !anchored {
                                <button
                                    ref={render.close_ref.clone()}
                                    type="button"
                                    class="app-sidebar-close app-compact-control"
                                    aria-label={close_label}
                                    onclick={close_click}
                                >{"Close"}</button>
                            }
                        </header>
                        {render.content.body}
                    </div>
                </div>
            </aside>
        </div>
    }
}
