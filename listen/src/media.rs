//! Typed vendor-content decomposition and generated-Markdown media normalization.

use serde_json::Value;
use wire::Media;

use crate::{ExternalMedia, ResidueKind};

/// The authority that permits one value to expose media.
pub(crate) enum OwnedContent<'a> {
    /// Authored text may carry retained canonical Markdown media.
    AuthoredText(&'a str),
    /// Provider-owned structure may carry typed inline image blocks.
    ProviderValue {
        value: &'a Value,
        default_name: &'a str,
    },
}

pub(crate) struct Decomposition {
    pub(crate) text: String,
    pub(crate) media: Vec<Media>,
    pub(crate) external_media: Vec<ExternalMedia>,
}

enum Candidate {
    Inline(Media),
    External(ExternalMedia),
}

/// Decompose one owned current content value through the sole media contract.
///
/// Provider strings remain exact output text and are never reparsed as JSON or
/// Markdown. Only authored text can expose retained Markdown media, while only
/// typed provider objects can expose inline media.
pub(crate) fn decompose(content: OwnedContent<'_>) -> Result<Decomposition, ResidueKind> {
    match content {
        OwnedContent::AuthoredText(text) => {
            let media = wire::media::parse_markdown(text)
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| malformed())?;
            Ok(Decomposition {
                text: text.to_owned(),
                media: unique(media)?,
                external_media: Vec::new(),
            })
        }
        OwnedContent::ProviderValue {
            value,
            default_name,
        } => {
            let mut text = Vec::new();
            let mut candidates = Vec::new();
            collect(value, default_name, &mut text, &mut candidates)?;
            let (media, external_media) = unique_candidates(candidates)?;
            Ok(Decomposition {
                text: text.join("\n"),
                media,
                external_media,
            })
        }
    }
}

fn collect(
    value: &Value,
    default_name: &str,
    text: &mut Vec<String>,
    media: &mut Vec<Candidate>,
) -> Result<(), ResidueKind> {
    match value {
        Value::Array(values) => {
            for value in values {
                collect(value, default_name, text, media)?;
            }
        }
        Value::String(value) => {
            text.push(value.clone());
        }
        Value::Object(object) => {
            if let Some(candidate) = image(object, default_name)? {
                media.push(candidate);
                return Ok(());
            }
            if let Some(value) = object
                .get("text")
                .or_else(|| object.get("content").filter(|value| value.is_string()))
                .and_then(Value::as_str)
            {
                text.push(value.to_owned());
                return Ok(());
            }
            for field in ["content", "output", "result", "Ok", "Err"] {
                if let Some(value) = object.get(field) {
                    collect(value, default_name, text, media)?;
                }
            }
        }
        Value::Null => {}
        Value::Bool(value) => text.push(value.to_string()),
        Value::Number(value) => text.push(value.to_string()),
    }
    Ok(())
}

fn image(
    object: &serde_json::Map<String, Value>,
    default_name: &str,
) -> Result<Option<Candidate>, ResidueKind> {
    let owned = object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "image" | "input_image" | "output_image"));
    if !owned {
        return if claims_media_payload(object) {
            Err(malformed())
        } else {
            Ok(None)
        };
    }

    let name = one_field(object, &["name", "filename"])?
        .map(|value| value.as_str().ok_or_else(malformed))
        .transpose()?
        .unwrap_or(default_name);
    let url = one_field(object, &["data_url", "image_url", "url"])?;
    let source = object.get("source");
    let data = object.get("data");
    let content_type = one_field(object, &["mimeType", "mime_type", "media_type"])?;
    match (url, source, data, content_type) {
        (Some(url), None, None, None) => {
            let url = url.as_str().ok_or_else(malformed)?;
            admit(name, url).map(|media| Some(Candidate::Inline(media)))
        }
        (None, Some(source), None, None) => {
            let source = source.as_object().ok_or_else(malformed)?;
            if source.get("type").and_then(Value::as_str) != Some("base64") {
                return Err(malformed());
            }
            let content_type = source
                .get("media_type")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            let data = source
                .get("data")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            Media::from_base64(name, content_type, data)
                .map(|media| Some(Candidate::Inline(media)))
                .map_err(|_| malformed())
        }
        (None, None, Some(data), Some(content_type)) => {
            let data = data.as_str().ok_or_else(malformed)?;
            let content_type = content_type.as_str().ok_or_else(malformed)?;
            if data.starts_with("blob:") {
                return ExternalMedia::admit(name, content_type, data)
                    .map(|media| Some(Candidate::External(media)))
                    .ok_or_else(malformed);
            }
            Media::from_base64(name, content_type, data)
                .map(|media| Some(Candidate::Inline(media)))
                .map_err(|_| malformed())
        }
        _ => Err(malformed()),
    }
}

fn one_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    fields: &[&str],
) -> Result<Option<&'a Value>, ResidueKind> {
    let mut values = fields.iter().filter_map(|field| object.get(*field));
    let value = values.next();
    if values.next().is_some() {
        return Err(malformed());
    }
    Ok(value)
}

fn claims_media_payload(object: &serde_json::Map<String, Value>) -> bool {
    object.contains_key("data_url")
        || object.contains_key("image_url")
        || object.get("source").is_some_and(|source| {
            source.as_object().is_some_and(|source| {
                source.get("type").and_then(Value::as_str) == Some("base64")
                    || source.contains_key("media_type")
            })
        })
        || (object.contains_key("data")
            && ["mimeType", "mime_type", "media_type"]
                .iter()
                .any(|field| object.contains_key(*field)))
}

fn admit(name: &str, data_url: &str) -> Result<Media, ResidueKind> {
    let content_type = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,").map(|(kind, _)| kind))
        .ok_or_else(malformed)?;
    Media::admit(name, content_type, data_url).map_err(|_| malformed())
}

fn unique_candidates(
    mut candidates: Vec<Candidate>,
) -> Result<(Vec<Media>, Vec<ExternalMedia>), ResidueKind> {
    candidates.truncate(wire::media::MAX_MEDIA_ITEMS);
    let mut media = Vec::new();
    let mut external_media = Vec::new();
    for candidate in candidates {
        match candidate {
            Candidate::Inline(candidate) => {
                let identity = candidate.identity().map_err(|_| malformed())?;
                if !media.iter().any(|existing: &Media| {
                    existing
                        .identity()
                        .is_ok_and(|existing| existing == identity)
                }) {
                    media.push(candidate);
                }
            }
            Candidate::External(candidate) => {
                if !external_media.contains(&candidate) {
                    external_media.push(candidate);
                }
            }
        }
    }
    wire::media::validate_media(&media).map_err(|_| malformed())?;
    Ok((media, external_media))
}

/// Keep the first wire-representable source-order media prefix.
///
/// Every candidate has already passed individual shape admission while it was
/// collected, so truncation bounds projection without hiding malformed input.
pub(crate) fn unique(mut media: Vec<Media>) -> Result<Vec<Media>, ResidueKind> {
    media.truncate(wire::media::MAX_MEDIA_ITEMS);
    wire::media::validate_media(&media).map_err(|_| malformed())?;
    let mut unique = Vec::<Media>::new();
    for candidate in media {
        let identity = candidate.identity().map_err(|_| malformed())?;
        let duplicate = unique.iter().any(|existing| {
            existing
                .identity()
                .is_ok_and(|existing| existing == identity)
        });
        if !duplicate {
            unique.push(candidate);
        }
    }
    Ok(unique)
}

fn malformed() -> ResidueKind {
    ResidueKind::Malformed {
        field: "media",
        expected: "at most eight canonical PNG, JPEG, GIF, or WebP data blocks",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &str = "iVBORw0KGgo=";

    #[test]
    fn retained_and_inline_media_share_one_decomposition_contract() {
        let url = format!("data:image/png;base64,{PNG}");
        let markdown = format!("![capture.png]({url})");
        let inline = serde_json::json!({
            "type": "image",
            "data": PNG,
            "mimeType": "image/png",
            "name": "echo.png",
        });
        let retained = decompose(OwnedContent::AuthoredText(&markdown)).unwrap();
        let inline = decompose(OwnedContent::ProviderValue {
            value: &inline,
            default_name: "tool-image.png",
        })
        .unwrap();
        assert_eq!(retained.text, markdown);
        assert_eq!(inline.text, "");
        assert_eq!(retained.media.len(), 1);
        assert_eq!(inline.media.len(), 1);
        assert_eq!(
            retained.media[0].identity().unwrap(),
            inline.media[0].identity().unwrap(),
        );
    }

    #[test]
    fn externalized_provider_media_is_retained_as_a_typed_reference() {
        let block = serde_json::json!({
            "type": "image",
            "data": format!("blob:sha256:{}", "a".repeat(64)),
            "mimeType": "image/png",
            "name": "capture.png",
        });
        let content = decompose(OwnedContent::ProviderValue {
            value: &block,
            default_name: "tool-image",
        })
        .expect("canonical blob reference decodes");
        assert!(content.media.is_empty());
        assert_eq!(content.external_media.len(), 1);
        let reference = &content.external_media[0];
        assert_eq!(reference.name(), "capture.png");
        assert_eq!(reference.content_type(), "image/png");
        assert_eq!(reference.sha256(), "a".repeat(64));
    }

    #[test]
    fn externalized_provider_media_requires_exact_lowercase_sha256_uri() {
        for data in [
            "blob:sha256:abc".to_owned(),
            format!("blob:sha256:{}", "A".repeat(64)),
            format!("blob:sha256:{}x", "a".repeat(64)),
            format!("file:sha256:{}", "a".repeat(64)),
        ] {
            let block = serde_json::json!({
                "type": "image",
                "data": data,
                "mimeType": "image/png",
            });
            assert!(matches!(
                decompose(OwnedContent::ProviderValue {
                    value: &block,
                    default_name: "tool-image.png",
                }),
                Err(ResidueKind::Malformed { field: "media", .. })
            ));
        }
    }

    #[test]
    fn strings_are_opaque_even_when_they_spell_media() {
        let url = format!("data:image/png;base64,{PNG}");
        let text = format!("![capture.png]({url})\n{{\"type\":\"image\",\"image_url\":\"{url}\"}}");
        let value = Value::String(text.clone());
        let content = decompose(OwnedContent::ProviderValue {
            value: &value,
            default_name: "tool-image.png",
        })
        .unwrap();
        assert_eq!(content.text, text);
        assert!(content.media.is_empty());
    }

    #[test]
    fn malformed_typed_image_is_a_media_error() {
        let block = serde_json::json!({"type": "image", "data": PNG});
        assert!(matches!(
            decompose(OwnedContent::ProviderValue {
                value: &block,
                default_name: "tool-image.png",
            }),
            Err(ResidueKind::Malformed { field: "media", .. })
        ));
    }

    #[test]
    fn ambiguous_media_fields_do_not_create_a_resolver_order() {
        let url = format!("data:image/png;base64,{PNG}");
        let blocks = [
            serde_json::json!({
                "type": "input_image",
                "data_url": url,
                "image_url": url,
            }),
            serde_json::json!({
                "type": "image",
                "data": PNG,
                "mimeType": "image/png",
                "media_type": "image/png",
            }),
        ];
        for block in blocks {
            assert!(matches!(
                decompose(OwnedContent::ProviderValue {
                    value: &block,
                    default_name: "tool-image.png",
                }),
                Err(ResidueKind::Malformed { field: "media", .. })
            ));
        }
    }

    #[test]
    fn canonical_provider_media_shapes_share_one_identity() {
        let url = format!("data:image/png;base64,{PNG}");
        let blocks = [
            serde_json::json!({"type": "image", "data_url": url}),
            serde_json::json!({
                "type": "image",
                "source": {"type": "base64", "media_type": "image/png", "data": PNG},
            }),
            serde_json::json!({
                "type": "image",
                "data": PNG,
                "mimeType": "image/png",
            }),
        ];
        let media = blocks
            .iter()
            .map(|block| {
                let content = decompose(OwnedContent::ProviderValue {
                    value: block,
                    default_name: "tool-image.png",
                })
                .unwrap();
                assert_eq!(content.media.len(), 1);
                content.media.into_iter().next().unwrap()
            })
            .collect::<Vec<_>>();
        for candidate in &media[1..] {
            assert_eq!(media[0].identity().unwrap(), candidate.identity().unwrap());
        }
    }

    #[test]
    fn cross_shape_media_fields_refuse_without_resolver_order() {
        let url = format!("data:image/png;base64,{PNG}");
        let source = serde_json::json!({
            "type": "base64",
            "media_type": "image/png",
            "data": PNG,
        });
        let blocks = [
            serde_json::json!({"type": "image", "data_url": url, "data": PNG}),
            serde_json::json!({
                "type": "image",
                "data_url": url,
                "mimeType": "image/jpeg",
            }),
            serde_json::json!({"type": "image", "source": source, "data": PNG}),
            serde_json::json!({"type": "image", "source": source, "image_url": url}),
            serde_json::json!({
                "type": "image",
                "source": source,
                "mimeType": "image/png",
            }),
        ];
        for block in blocks {
            assert!(matches!(
                decompose(OwnedContent::ProviderValue {
                    value: &block,
                    default_name: "tool-image.png",
                }),
                Err(ResidueKind::Malformed { field: "media", .. })
            ));
        }
    }

    #[test]
    fn unowned_media_payload_is_not_reclassified_as_text() {
        let block = serde_json::json!({
            "type": "text",
            "text": "fallback prose",
            "image_url": format!("data:image/png;base64,{PNG}"),
        });
        assert!(matches!(
            decompose(OwnedContent::ProviderValue {
                value: &block,
                default_name: "tool-image.png",
            }),
            Err(ResidueKind::Malformed { field: "media", .. })
        ));
    }

    #[test]
    fn malformed_retained_media_is_a_media_error() {
        assert!(matches!(
            decompose(OwnedContent::AuthoredText(
                "![capture.png](data:image/png;base64,%%%)",
            )),
            Err(ResidueKind::Malformed { field: "media", .. })
        ));
    }
}
