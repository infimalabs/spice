//! Safe authored text and trusted line boundaries for generated Mermaid labels.

/// Escape authored label text while retaining intentional renderer line breaks.
pub(crate) fn multiline(value: &str) -> String {
    let mut label = String::with_capacity(value.len() + 16);
    for (index, line) in value.lines().enumerate() {
        if index != 0 {
            label.push_str("<br/>");
        }
        for character in line.chars() {
            match character {
                '&' => label.push_str("&amp;"),
                '"' => label.push_str("&quot;"),
                '<' => label.push_str("&lt;"),
                '>' => label.push_str("&gt;"),
                character => label.push(character),
            }
        }
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_semantic_lines_and_escapes_authored_markup() {
        assert_eq!(
            multiline("TASK-1\nReview <this> & \"that\"\nPhase todo — ready"),
            "TASK-1<br/>Review &lt;this&gt; &amp; &quot;that&quot;<br/>Phase todo — ready"
        );
    }
}
