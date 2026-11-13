//! Message-card presentation inside a surface that already owns card identity.

use super::*;

/// Render the exact message presentation without duplicating the team-plane
/// card's DOM identity, routing, or lattice placement.
pub(super) fn render(
    props: &AgentMessageCardProps,
    correspondent: &view::chrome::Correspondent,
) -> Html {
    let message = &props.message;
    let AgentMessagePresentation {
        attribution,
        footer_label,
        is_boundary,
        rendered_html,
        unlinked_media,
        media_description,
        image_only,
        lone_tool_image,
        single_image_cover,
        card_label,
        ..
    } = agent_message_presentation(props, correspondent);
    if is_boundary {
        return render_agent_boundary(message, &footer_label);
    }
    let speaking = props.speech_owner.is_some();
    let operator = message.activity == wire::AgentActivityKind::Direction;
    let accent = (!operator).then(|| correspondent.accent.to_string());
    html! {
        <div class="team-surface room-inbox-message-surface">
            <article
                class={classes!(
                    "mosaic-message-card",
                    "room-inbox-message-card",
                    image_only.then_some("mosaic-message-card--image-only"),
                    single_image_cover.then_some("mosaic-message-card--image-cover"),
                    message.final_answer.then_some("final"),
                    speaking.then_some("now-playing"),
                )}
                aria-label={card_label}
                tabindex="-1"
                data-card-kind="agent-message"
                data-message-role={if operator { "operator" } else { "agent" }}
                data-layout-phase="embedded"
                data-agent-lane={props.member.lane.to_string()}
                data-agent-activity={watchband::agent_activity_name(message.activity)}
                data-speech-active={speaking.to_string()}
                data-producer-target-id={props.quote_lane.to_string()}
                data-accent={accent.clone()}
                data-accent-slot={accent}
            >
                <div class="mosaic-message-content">
                    {render_normal_face(message, NormalFace {
                        rendered_html,
                        unlinked_media,
                        media_description: media_description.as_deref(),
                        lone_tool_image,
                        crystal: None,
                        footer_label: &footer_label,
                        attribution: &attribution,
                    })}
                </div>
            </article>
        </div>
    }
}
