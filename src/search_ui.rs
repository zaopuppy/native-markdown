//! Unicode search ranges shared by document analysis.
use std::ops::Range;

/// Original byte ranges, even when Unicode lowercase changes byte length.
pub fn search_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return vec![];
    }
    let mut folded = String::new();
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    for (start, ch) in text.char_indices() {
        for c in ch.to_lowercase() {
            folded.push(c);
            starts.extend(std::iter::repeat_n(start, c.len_utf8()));
            ends.extend(std::iter::repeat_n(start + ch.len_utf8(), c.len_utf8()));
        }
    }
    let mut ranges: Vec<Range<usize>> = Vec::new();
    for (start, _) in folded.match_indices(&query) {
        let range = starts[start]..ends[start + query.len() - 1];
        if ranges.last() != Some(&range) {
            ranges.push(range);
        }
    }
    ranges
}
