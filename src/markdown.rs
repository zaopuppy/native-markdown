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
    pub source_offset: Option<usize>,
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
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }

    let mut hits = Vec::new();
    for (section_index, section) in sections(markdown, headings).iter().enumerate() {
        let section_source = &markdown[section.range.clone()];
        let lowercase = plain_text_with_mermaid(section_source, false).to_lowercase();
        for (offset, _) in lowercase.match_indices(&query) {
            let start = lowercase[..offset]
                .char_indices()
                .rev()
                .nth(24)
                .map_or(0, |(index, _)| index);
            let tail = (offset + query.len()).min(lowercase.len());
            let end = lowercase[tail..]
                .char_indices()
                .nth(36)
                .map_or(lowercase.len(), |(index, _)| tail + index);
            let snippet = lowercase[start..end]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            hits.push(SearchHit {
                section_index,
                snippet,
                source_offset: None,
            });
        }

        for range in mermaid_body_ranges(section_source) {
            let body = &section_source[range.clone()];
            let (lowercase, source_offsets) = lowercase_with_source_offsets(body);
            for (offset, _) in lowercase.match_indices(&query) {
                let start = lowercase[..offset]
                    .char_indices()
                    .rev()
                    .nth(24)
                    .map_or(0, |(index, _)| index);
                let tail = (offset + query.len()).min(lowercase.len());
                let end = lowercase[tail..]
                    .char_indices()
                    .nth(36)
                    .map_or(lowercase.len(), |(index, _)| tail + index);
                hits.push(SearchHit {
                    section_index,
                    snippet: lowercase[start..end]
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    source_offset: Some(section.range.start + range.start + source_offsets[offset]),
                });
            }
        }
    }
    hits
}

fn lowercase_with_source_offsets(source: &str) -> (String, Vec<usize>) {
    let mut lowercase = String::new();
    let mut source_offsets = Vec::new();
    for (source_offset, character) in source.char_indices() {
        for folded in character.to_lowercase() {
            lowercase.push(folded);
            source_offsets.extend(std::iter::repeat_n(source_offset, folded.len_utf8()));
        }
    }
    source_offsets.push(source.len());
    (lowercase, source_offsets)
}

fn mermaid_body_ranges(markdown: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut in_mermaid = false;
    let mut body_start = None;
    let mut body_end = 0;
    for (event, range) in Parser::new_ext(markdown, parser_options()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.trim().eq_ignore_ascii_case("mermaid") =>
            {
                in_mermaid = true;
                body_start = None;
                body_end = 0;
            }
            Event::Text(_) if in_mermaid => {
                body_start.get_or_insert(range.start);
                body_end = range.end;
            }
            Event::End(TagEnd::CodeBlock) if in_mermaid => {
                in_mermaid = false;
                let start = body_start.take().unwrap_or(range.start);
                ranges.push(start..body_end.max(start));
            }
            _ => {}
        }
    }
    ranges
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
