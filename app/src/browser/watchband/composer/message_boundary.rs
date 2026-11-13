use super::*;

pub(super) fn render(message: &wire::AgentMessage, attribution: &str) -> Html {
    let (event, rendered_event) = if message.activity == wire::AgentActivityKind::Session {
        (
            "started fresh session".to_owned(),
            html! { "started fresh session" },
        )
    } else if let Some(method) = message.compaction_method.as_deref() {
        let (method, separator) = if method == "snapcompact" {
            ("snap", "")
        } else {
            (method, " ")
        };
        (
            format!("{method}{separator}compacted context"),
            html! {
                <>
                    <span class="mosaic-compaction-method">{method.to_owned()}</span>
                    {separator}
                    {"compacted context"}
                </>
            },
        )
    } else {
        (
            "compacted context".to_owned(),
            html! { "compacted context" },
        )
    };
    let link_label = format!("Link to {event} from {attribution}");
    let attribution = attribution.to_owned();
    let display_millis = message.display_millis();
    let href = crate::view::card_href(&agent_message_source_id(message));
    html! {
        <>
            <span class="mosaic-compaction-rule" aria-hidden="true"></span>
            <span class="mosaic-compaction-label">
                <Age
                    authored={display_millis}
                    class="mosaic-compaction-age"
                    {href}
                    label={link_label}
                />
                <span class="mosaic-compaction-context">
                    <span class="mosaic-compaction-agent" title={attribution.clone()}>{attribution}</span>
                    <span class="message-key">{rendered_event}</span>
                </span>
            </span>
            <span class="mosaic-compaction-rule" aria-hidden="true"></span>
        </>
    }
}
