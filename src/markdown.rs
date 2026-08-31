use std::ops::Range;

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

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

pub fn plain_text(markdown: &str) -> String {
    let mut output = String::new();
    for event in Parser::new_ext(markdown, parser_options()) {
        match event {
            Event::Text(text) | Event::Code(text) => {
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
    plain_text(markdown).split_whitespace().count()
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
        let lowercase = plain_text(&markdown[section.range.clone()]).to_lowercase();
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
            });
        }
    }
    hits
}

pub fn safe_preview_source(markdown: &str) -> String {
    let mut replacements = Vec::new();
    for (event, range) in Parser::new_ext(markdown, parser_options()).into_offset_iter() {
        match event {
            Event::Html(_) => replacements.push((range, true)),
            Event::InlineHtml(_) => replacements.push((range, false)),
            _ => {}
        }
    }

    if replacements.is_empty() {
        return markdown.to_owned();
    }

    let mut output = String::with_capacity(markdown.len() + replacements.len() * 8);
    let mut cursor = 0;
    for (range, block) in replacements {
        if range.start < cursor {
            continue;
        }
        output.push_str(&markdown[cursor..range.start]);
        let raw = markdown[range.clone()].trim_end();
        if block {
            output.push_str("\n```html\n");
            output.push_str(raw);
            output.push_str("\n```\n");
        } else {
            output.push('`');
            output.push_str(&raw.replace('`', "\\`"));
            output.push('`');
        }
        cursor = range.end;
    }
    output.push_str(&markdown[cursor..]);
    output
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
    fn raw_html_is_displayed_as_code() {
        let rendered = safe_preview_source("before <kbd>K</kbd> after");
        assert!(rendered.contains("`<kbd>`"));
        assert!(rendered.contains("`</kbd>`"));
    }

    #[test]
    fn parser_preserves_chinese_text() {
        let source = "# 1.理解大语言模型\n\n大语言模型可以生成新的文本。";
        let outline = headings(source);
        assert_eq!(outline[0].title, "1.理解大语言模型");
        assert!(plain_text(source).contains("大语言模型可以生成新的文本"));
    }
}
