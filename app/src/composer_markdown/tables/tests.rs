use crate::composer_markdown::render_received;

#[test]
fn aligned_table_cells_carry_alignment_without_an_inline_style() {
    let rendered = render_received(
        "| Name | Size | Mid |\n| :--- | ---: | :-: |\n| a | four | x |\n| b | twelve | y |",
    );

    assert!(
        !rendered.html.contains("style="),
        "app style-src refuses inline styles: {}",
        rendered.html
    );
    assert_eq!(
        rendered.html,
        concat!(
            "<div class=\"markdown-table\">",
            "<table><thead><tr><th data-align=\"left\">Name</th><th data-align=\"right\">Size</th>",
            "<th data-align=\"center\">Mid</th></tr></thead><tbody>\n",
            "<tr><td data-align=\"left\">a</td><td data-align=\"right\">four</td>",
            "<td data-align=\"center\">x</td></tr>\n",
            "<tr><td data-align=\"left\">b</td><td data-align=\"right\">twelve</td>",
            "<td data-align=\"center\">y</td></tr>\n",
            "</tbody></table>\n",
            "</div>",
        )
    );
}

/// Each column's squeeze policy covers every cell of that column, decided by
/// its longest word and longest cell anywhere in the table, header included.
#[test]
fn table_columns_are_measured_once_for_the_whole_table() {
    let long = "a".repeat(25);
    let edge = "b".repeat(24);
    let rendered = render_received(&format!(
        "| Window | MDS | Path | Edge | Label |\n| --- | :-: | --- | --- | --- |\n\
         | 120 s with no build of mine | 3.13 | `{long}` | {edge} | twenty four chars here!! |\n\
         | Fresh | 0.7–0.9 | short | x | ok |"
    ));
    let html = &rendered.html;

    assert!(
        html.contains("<th data-measure=\"prose\">Window</th>")
            && html.contains("<td data-measure=\"prose\">Fresh</td>"),
        "a column with a spaced cell over 24 characters keeps a prose floor in every row: {html}"
    );
    assert!(
        html.contains("<th data-align=\"center\">MDS</th>")
            && html.contains("<td data-align=\"center\">0.7–0.9</td>"),
        "short cells stay plain and keep their alignment: {html}"
    );
    assert!(
        html.contains("<th data-measure=\"token\">Path</th>")
            && html.contains("<td data-measure=\"token\">short</td>"),
        "a word longer than 24 characters, code included, lets its column break it: {html}"
    );
    assert!(
        html.contains("<th>Edge</th>") && html.contains("<th>Label</th>"),
        "a 24-character word and a 24-character spaced cell stay plain: {html}"
    );
}
