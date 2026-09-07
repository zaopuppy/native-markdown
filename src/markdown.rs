use std::ops::Range;

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Heading {
    pub level: u8,
    pub title: String,
    pub offset: usize,
}

#[derive(Clone, Debug)]
pub struct Section {
    pub heading_index: Option<usize>,
    pub range: Range<usize>,
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub section_index: usize,
    pub snippet: String,
    pub match_range: Range<usize>,
    pub source_offset: Option<usize>,
    pub source_end: usize,
}

pub fn parser_options() -> Options {
    Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_GFM
}

pub fn headings(markdown: &str) -> Vec<Heading> {
    let mut result = Vec::new();
    let mut current: Option<(u8, usize, String)> = None;

    for (event, range) in Parser::new_ext(markdown, parser_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((heading_level(level), range.start, String::new()));
            }
            Event::Text(text) | Event::Code(text) if current.is_some() => {
                current.as_mut().unwrap().2.push_str(&text);
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((level, offset, title)) = current.take() {
                    let title = title.trim().to_owned();
                    if !title.is_empty() {
                        result.push(Heading {
                            level,
                            title,
                            offset,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    result
}

pub fn sections(markdown: &str, headings: &[Heading]) -> Vec<Section> {
    if headings.is_empty() {
        return vec![Section {
            heading_index: None,
            range: 0..markdown.len(),
        }];
    }

    let mut result = Vec::new();
    if headings[0].offset > 0 {
        result.push(Section {
            heading_index: None,
            range: 0..headings[0].offset,
        });
    }

    for (index, heading) in headings.iter().enumerate() {
        let end = headings
            .get(index + 1)
            .map_or(markdown.len(), |next| next.offset);
        result.push(Section {
            heading_index: Some(index),
            range: heading.offset..end,
        });
    }

    result
}

pub fn focus_range(
    markdown: &str,
    headings: &[Heading],
    section_index: usize,
) -> Option<Range<usize>> {
    let section = sections(markdown, headings).get(section_index)?.clone();
    let Some(heading_index) = section.heading_index else {
        return Some(section.range);
    };
    let heading = headings.get(heading_index)?;
    let end = headings
        .iter()
        .skip(heading_index + 1)
        .find(|candidate| candidate.level <= heading.level)
        .map_or(markdown.len(), |candidate| candidate.offset);

    Some(heading.offset..end)
}

fn reading_text(markdown: &str) -> String {
    plain_text_with_mermaid(markdown, false)
}

fn plain_text_with_mermaid(markdown: &str, include_mermaid: bool) -> String {
    let mut output = String::new();
    let mut in_mermaid = false;
    for event in Parser::new_ext(markdown, parser_options()) {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info))) => {
                in_mermaid = info.trim().eq_ignore_ascii_case("mermaid");
            }
            Event::End(TagEnd::CodeBlock) => in_mermaid = false,
            Event::Text(text) | Event::Code(text) if include_mermaid || !in_mermaid => {
                output.push_str(&text);
                output.push(' ');
            }
            Event::SoftBreak | Event::HardBreak => output.push('\n'),
            Event::Html(html) | Event::InlineHtml(html) => {
                output.push_str(&html);
                output.push(' ');
            }
            _ => {}
        }
    }
    output
}

pub fn word_count(markdown: &str) -> usize {
    reading_text(markdown).split_whitespace().count()
}

pub fn reading_minutes(words: usize) -> usize {
    words.max(1).div_ceil(220)
}

pub fn search(markdown: &str, query: &str, headings: &[Heading]) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    if query.trim().is_empty() {
        return hits;
    }
    let sections = sections(markdown, headings);
    let mut text = String::new();
    let mut positions: Vec<Range<usize>> = Vec::new();
    let mut image_depth = 0;
    let flush =
        |text: &mut String, positions: &mut Vec<Range<usize>>, hits: &mut Vec<SearchHit>| {
            for range in gpui_component::text::search_ranges(text, query) {
                let offset = positions[range.start].start;
                let source_end = positions[range.end - 1].end;
                let (snippet, match_range) = search_snippet(text, range.start, range.len());
                let section_index = sections
                    .iter()
                    .position(|s| s.range.contains(&offset))
                    .unwrap_or(0);
                hits.push(SearchHit {
                    section_index,
                    snippet,
                    match_range,
                    source_offset: Some(offset),
                    source_end,
                });
            }
            text.clear();
            positions.clear();
        };
    for (event, source_range) in Parser::new_ext(markdown, parser_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::Image { .. }) => {
                flush(&mut text, &mut positions, &mut hits);
                image_depth += 1;
            }
            Event::End(TagEnd::Image) => {
                image_depth -= 1;
            }
            Event::Text(value) | Event::Code(value) if image_depth == 0 => {
                let source = &markdown[source_range.clone()];
                if source.starts_with('&') && source.ends_with(';') && value.as_ref() != source {
                    positions.extend(std::iter::repeat_n(source_range, value.len()));
                    text.push_str(&value);
                    continue;
                }
                let mut cursor = 0;
                for ch in value.chars() {
                    // Parser offsets keep hidden link URLs and Markdown delimiters out of navigation.
                    let local = source[cursor..]
                        .find(ch)
                        .map(|n| cursor + n)
                        .unwrap_or(cursor);
                    let end = (local + ch.len_utf8()).min(source.len());
                    positions.extend(std::iter::repeat_n(
                        source_range.start + local..source_range.start + end,
                        ch.len_utf8(),
                    ));
                    text.push(ch);
                    cursor = end;
                    while !source.is_char_boundary(cursor) {
                        cursor += 1;
                    }
                }
            }
            Event::SoftBreak | Event::HardBreak if image_depth == 0 => {
                text.push('\n');
                positions.push(source_range);
            }
            Event::End(
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::TableCell | TagEnd::CodeBlock,
            ) => flush(&mut text, &mut positions, &mut hits),
            _ => {}
        }
    }
    flush(&mut text, &mut positions, &mut hits);
    hits
}

fn search_snippet(text: &str, offset: usize, match_len: usize) -> (String, Range<usize>) {
    let start = text[..offset]
        .char_indices()
        .rev()
        .nth(24)
        .map_or(0, |(index, _)| index);
    let tail = (offset + match_len).min(text.len());
    let end = text[tail..]
        .char_indices()
        .nth(36)
        .map_or(text.len(), |(index, _)| tail + index);
    let prefix = normalize_whitespace(&text[start..offset]);
    let matched = normalize_whitespace(&text[offset..tail]);
    let suffix = normalize_whitespace(&text[tail..end]);
    let prefix_separator = (!prefix.is_empty()
        && text[..offset]
            .chars()
            .next_back()
            .is_some_and(char::is_whitespace)) as usize;
    let suffix_separator = (!suffix.is_empty()
        && text[tail..].chars().next().is_some_and(char::is_whitespace))
        as usize;
    let match_start = prefix.len() + prefix_separator;
    let match_end = match_start + matched.len();
    (
        format!(
            "{prefix}{}{matched}{}{suffix}",
            " ".repeat(prefix_separator),
            " ".repeat(suffix_separator)
        ),
        match_start..match_end,
    )
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_atx_and_setext_headings() {
        let source = "# One\n\nTwo\n---\n\n### Three";
        let found = headings(source);
        assert_eq!(
            found
                .iter()
                .map(|heading| (heading.title.as_str(), heading.level))
                .collect::<Vec<_>>(),
            vec![("One", 1), ("Two", 2), ("Three", 3)]
        );
    }

    #[test]
    fn search_counts_visible_text_by_section() {
        let source = "# Alpha\nneedle here\n\n# Beta\nneedle again";
        let outline = headings(source);
        let hits = search(source, "needle", &outline);
        assert_eq!(hits.len(), 2);
        assert_ne!(hits[0].section_index, hits[1].section_index);
    }

    #[test]
    fn search_preserves_snippet_casing_and_marks_the_match_range() {
        let source = "# Notes\nThe OpenAI API is ready.";
        let outline = headings(source);
        let hits = search(source, "api", &outline);

        assert_eq!(hits.len(), 1);
        assert!(hits[0].snippet.contains("OpenAI API"));
        assert_eq!(&hits[0].snippet[hits[0].match_range.clone()], "API");
        assert_eq!(&source[hits[0].source_offset.unwrap()..][..3], "API");
    }

    #[test]
    fn search_maps_visible_unicode_and_formatted_text_to_source() {
        let source = "# Test\n\n[link](https://needle.example) **Ne**edle, İSTANBUL, 中文匹配.\n\n```mermaid\nflowchart LR\nneedle-->B\n```\n\nLast Needle.";
        let outline = headings(source);
        let hits = search(source, "needle", &outline);
        assert_eq!(hits.len(), 3);
        assert_eq!(
            &source[hits[0].source_offset.unwrap()..hits[0].source_end],
            "Ne**edle"
        );
        assert_eq!(
            &source[hits[1].source_offset.unwrap()..hits[1].source_end],
            "needle"
        );
        assert_eq!(
            &source[hits[2].source_offset.unwrap()..hits[2].source_end],
            "Needle"
        );
        for query in ["i", "stanbul", "中文", "匹配"] {
            for hit in search(source, query, &outline) {
                assert!(source.is_char_boundary(hit.source_offset.unwrap()));
                assert!(source.is_char_boundary(hit.source_end));
                assert!(!hit.snippet[hit.match_range].is_empty());
            }
        }
    }

    #[test]
    fn search_points_to_the_complete_encoded_entity() {
        let source = "# Entities\n\nA &amp; B.";
        let hits = search(source, "&", &headings(source));
        assert_eq!(hits.len(), 1);
        assert_eq!(
            &source[hits[0].source_offset.unwrap()..hits[0].source_end],
            "&amp;"
        );
    }

    #[test]
    fn focus_range_includes_the_complete_heading_subtree() {
        let source = "# Root\nroot\n## Child\nchild\n### Grandchild\ngrandchild\n## Peer\npeer\n# Next\nnext";
        let outline = headings(source);

        let root = focus_range(source, &outline, 0).unwrap();
        assert_eq!(
            &source[root],
            "# Root\nroot\n## Child\nchild\n### Grandchild\ngrandchild\n## Peer\npeer\n"
        );

        let child = focus_range(source, &outline, 1).unwrap();
        assert_eq!(
            &source[child],
            "## Child\nchild\n### Grandchild\ngrandchild\n"
        );
    }

    #[test]
    fn focus_range_handles_skipped_levels_and_the_last_heading() {
        let source = "Intro\n\n## Parent\nparent\n#### Skipped\nchild\n##### Last\nlast";
        let outline = headings(source);

        let parent = focus_range(source, &outline, 1).unwrap();
        assert_eq!(
            &source[parent],
            "## Parent\nparent\n#### Skipped\nchild\n##### Last\nlast"
        );

        let last = focus_range(source, &outline, 3).unwrap();
        assert_eq!(&source[last], "##### Last\nlast");
    }

    #[test]
    fn focus_range_preserves_flat_search_sections() {
        let source = "Intro needle\n\n# Parent\n## Child\nneedle";
        let outline = headings(source);
        let hits = search(source, "needle", &outline);

        assert_eq!(hits.len(), 2);
        assert_ne!(hits[0].section_index, hits[1].section_index);
        assert_eq!(
            &source[focus_range(source, &outline, hits[0].section_index).unwrap()],
            "Intro needle\n\n"
        );
    }

    #[test]
    fn search_marks_mermaid_source_hits_with_original_offsets() {
        let source = "# Diagram\n\n```mermaid\nflowchart LR\nAlpha-->Beta\n```";
        let outline = headings(source);
        let hits = search(source, "Alpha", &outline);
        assert_eq!(hits.len(), 1);
        let offset = hits[0].source_offset.unwrap();
        assert_eq!(&source[offset..offset + "Alpha".len()], "Alpha");
    }

    #[test]
    fn parser_preserves_chinese_text() {
        let source = "# 1.理解大语言模型\n\n大语言模型可以生成新的文本。";
        let outline = headings(source);
        assert_eq!(outline[0].title, "1.理解大语言模型");
        assert!(plain_text_with_mermaid(source, true).contains("大语言模型可以生成新的文本"));
    }

    #[test]
    fn word_count_excludes_mermaid_but_search_text_keeps_it() {
        let source = "Visible words\n\n```mermaid\nflowchart LR\nAlpha-->Beta\n```";
        assert_eq!(word_count(source), 2);
        assert!(plain_text_with_mermaid(source, true).contains("Alpha-->Beta"));
    }
}
