//! Identity matching and HTML controls for retained Markdown images.

use wire::MessageDirective;

pub(super) struct Matcher {
    quotes: Vec<wire::MediaQuote>,
    consumed: Vec<bool>,
}

impl Matcher {
    pub(super) fn new(media_quotes: &[wire::MediaQuote], directives: &[MessageDirective]) -> Self {
        let quotes = media_quotes
            .iter()
            .chain(
                directives
                    .iter()
                    .flat_map(|directive| directive.media_quotes.iter()),
            )
            .cloned()
            .collect::<Vec<_>>();
        let consumed = vec![false; quotes.len()];
        Self { quotes, consumed }
    }

    pub(super) fn take(&mut self, source: &str, name: &str) -> Option<&wire::MediaQuote> {
        let content_type = source.strip_prefix("data:")?.split_once(";base64,")?.0;
        let identity = wire::Media::admit(name, content_type, source)
            .and_then(|media| media.identity())
            .ok()?;
        let index = self
            .quotes
            .iter()
            .enumerate()
            .find(|(index, quote)| {
                !self.consumed[*index] && quote.name == name && *quote.target.digest() == identity
            })?
            .0;
        self.consumed[index] = true;
        self.quotes.get(index)
    }
}

pub(super) fn image_preview_html(
    source: &str,
    authored_title: &str,
    alt: &str,
    media_quote: Option<&wire::MediaQuote>,
) -> String {
    let source = super::escape_html_attribute(source);
    let alt = super::escape_html_attribute(alt);
    let label = if alt.is_empty() { "image" } else { &alt };
    let action = super::escape_html_attribute(&format!("Open {label} full size"));
    let image_title = if authored_title.is_empty() {
        String::new()
    } else {
        format!(
            " title=\"{}\"",
            super::escape_html_attribute(authored_title)
        )
    };
    let malformed_data_image = super::data_image_has_malformed_base64(&source);
    let (state, status) = if source.is_empty() || malformed_data_image {
        ("error", "Image unavailable")
    } else {
        ("pending", "Loading image")
    };
    let image = if source.is_empty() || malformed_data_image {
        format!("<img alt=\"{alt}\"{image_title} data-mosaic-image-state=\"{state}\">")
    } else {
        format!(
            "<img src=\"{source}\" alt=\"{alt}\"{image_title} \
             data-mosaic-image-state=\"{state}\">"
        )
    };
    let preview = format!(
        "{image}<span class=\"message-image-status\" role=\"status\" \
         data-mosaic-image-status=\"{state}\">{status}</span>"
    );
    let preview = if !source.is_empty() && !malformed_data_image {
        format!(
            "<button type=\"button\" class=\"message-image message-image--openable\" \
             data-image-preview=\"\" title=\"{action}\" aria-label=\"{action}\">{preview}</button>"
        )
    } else {
        format!("<span class=\"message-image\" title=\"Image preview\">{preview}</span>")
    };
    let quote = media_quote.map_or_else(String::new, |media_quote| {
        let target = super::escape_html_attribute(&media_quote.target.to_string());
        let name = super::escape_html_attribute(&media_quote.name);
        let action = super::escape_html_attribute(&format!("Quote {}", media_quote.name));
        let draft_action = super::escape_html_attribute(&format!(
            "Quote {} into the main composer",
            media_quote.name
        ));
        let (icon, draft_icon) = (super::QUOTE_ICON, super::DRAFT_QUOTE_ICON);
        let drawer = super::drawer_html(&format!(
            "<button type=\"button\" class=\"app-card-action app-card-action--draft-quote \
             message-image-quote message-image-quote--draft\" data-image-quote-action=\"draft\" \
             data-image-quote-target=\"{target}\" data-image-quote-name=\"{name}\" \
             title=\"{draft_action}\" aria-label=\"{draft_action}\">{draft_icon}</button>\
             <button type=\"button\" class=\"app-card-action app-card-action--quote \
             message-image-quote\" data-image-quote-action=\"\" \
             data-image-quote-target=\"{target}\" data-image-quote-name=\"{name}\" \
             title=\"{action}\" aria-label=\"{action}\">{icon}</button>"
        ));
        format!(
            "<span class=\"message-actions message-image-actions app-control-rail\">{drawer}</span>"
        )
    });
    format!("<span class=\"message-image-shell\">{preview}{quote}</span>")
}
