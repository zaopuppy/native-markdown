//! Clipboard import is an explicit user operation. No cache, polling or history files.
use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use base64::Engine as _;
use htmd::{
    element_handler::{Element, Handlers},
    HtmlToMarkdown,
};
use sha2::{Digest, Sha256};
use url::Url;

pub const MAX_IMAGE: usize = 20 * 1024 * 1024;
const MAX_TOTAL: usize = 100 * 1024 * 1024;
const MAX_IMAGES: usize = 50;
#[cfg(all(windows, not(test)))]
const MAX_CLIPBOARD: usize = 16 * 1024 * 1024;

/// Session-only limits captured when a background import starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadTimeouts {
    pub connect_secs: u64,
    pub image_secs: u64,
    pub batch_secs: u64,
}
impl Default for DownloadTimeouts {
    fn default() -> Self {
        Self {
            connect_secs: 5,
            image_secs: 10,
            batch_secs: 20,
        }
    }
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub html: Option<String>,
    pub source_url: Option<String>,
    pub text: Option<String>,
    pub images: Vec<Vec<u8>>,
    pub files: Vec<PathBuf>,
    pub rtf: Option<String>,
}

impl Snapshot {
    pub fn read(item: Option<&gpui::ClipboardItem>) -> Self {
        let mut result = Self::default();
        if let Some(item) = item {
            result.text = item.text();
            for entry in item.entries() {
                match entry {
                    gpui::ClipboardEntry::Image(image) => result.images.push(image.bytes.clone()),
                    gpui::ClipboardEntry::ExternalPaths(paths) => {
                        result.files.extend(paths.paths().iter().cloned())
                    }
                    _ => {}
                }
            }
        }
        #[cfg(all(windows, not(test)))]
        {
            if let Some(html) = read_format("HTML Format") {
                let (fragment, source) = html_fragment(&html);
                result.html = Some(fragment);
                result.source_url = source;
            }
            result.rtf = read_format("Rich Text Format");
        }
        result
    }

    pub fn is_rich(&self) -> bool {
        self.html.is_some()
            || !self.images.is_empty()
            || self.files.iter().any(|p| supported_path(p))
    }
}

/// CF_HTML offsets count UTF-8 bytes, not characters. Some producers only emit markers.
pub fn html_fragment(raw: &str) -> (String, Option<String>) {
    let source = raw
        .lines()
        .find_map(|line| line.strip_prefix("SourceURL:").map(str::trim))
        .map(str::to_owned);
    let offset = |key: &str| {
        raw.lines().find_map(|l| {
            l.strip_prefix(key)
                .and_then(|v| v.trim().parse::<usize>().ok())
        })
    };
    let fragment = offset("StartFragment:")
        .zip(offset("EndFragment:"))
        .and_then(|(a, b)| raw.get(a..b))
        .or_else(|| {
            let a = raw.find("<!--StartFragment-->")? + "<!--StartFragment-->".len();
            let b = raw[a..].find("<!--EndFragment-->")? + a;
            raw.get(a..b)
        })
        .unwrap_or_else(|| raw.find('<').and_then(|i| raw.get(i..)).unwrap_or(raw));
    (fragment.trim_end_matches('\0').to_owned(), source)
}

#[cfg(all(windows, not(test)))]
fn read_format(name: &str) -> Option<String> {
    use windows_sys::Win32::System::{DataExchange::*, Memory::*};
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let format = RegisterClipboardFormatW(name.as_ptr());
        if format == 0 || OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        struct Close;
        impl Drop for Close {
            fn drop(&mut self) {
                unsafe {
                    CloseClipboard();
                }
            }
        }
        let _close = Close;
        let handle = GetClipboardData(format);
        if handle.is_null() {
            return None;
        }
        let length = GlobalSize(handle);
        if length == 0 || length > MAX_CLIPBOARD {
            return None;
        }
        let pointer = GlobalLock(handle);
        if pointer.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(pointer as *const u8, length);
        let result = String::from_utf8_lossy(bytes)
            .trim_end_matches('\0')
            .to_owned();
        GlobalUnlock(handle);
        Some(result)
    }
}

#[derive(Clone)]
enum Source {
    Bytes(Vec<u8>),
    Location(String),
}
#[derive(Clone)]
struct Picture {
    token: String,
    alt: String,
    source: Source,
    link: Option<String>,
    keep_failed_link: bool,
}
pub struct Import {
    pub markdown: String,
    pictures: Vec<Picture>,
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
fn destination(value: &str) -> String {
    value
        .replace('%', "%25")
        .replace(' ', "%20")
        .replace('(', "%28")
        .replace(')', "%29")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('\\', "%5C")
        .replace('#', "%23")
        .replace(['\r', '\n'], "")
}
fn external_url(value: &str) -> Option<Url> {
    Url::parse(value).ok().filter(|u| {
        matches!(u.scheme(), "http" | "https") && u.username().is_empty() && u.password().is_none()
    })
}

fn convert_table(handlers: &dyn Handlers, node: &htmd::node::Node<'_>) -> String {
    fn rows(
        handlers: &dyn Handlers,
        node: &htmd::node::Node<'_>,
        output: &mut Vec<Vec<String>>,
        depth: usize,
    ) {
        if depth > 100 {
            return;
        }
        if node.tag_name() == Some("tr") {
            let mut cells = Vec::new();
            for cell in node
                .children()
                .iter()
                .filter(|c| matches!(c.tag_name(), Some("td" | "th")))
            {
                cells.push(
                    handlers
                        .walk_children(&cell, htmd::element_handler::Context::INLINE)
                        .content
                        .trim()
                        .replace('|', "\\|")
                        .replace('\n', "<br>"),
                );
                let span = cell
                    .attr("colspan")
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1)
                    .clamp(1, 32);
                cells.extend((1..span).map(|_| String::new()));
            }
            if !cells.is_empty() {
                output.push(cells);
            }
        } else {
            for child in node.children().iter() {
                rows(handlers, &child, output, depth + 1);
            }
        }
    }
    let mut table = Vec::new();
    rows(handlers, node, &mut table, 0);
    let columns = table.iter().map(Vec::len).max().unwrap_or(0);
    if columns == 0 {
        return String::new();
    }
    let mut result = String::from("\n\n");
    for (index, row) in table.iter().enumerate() {
        result.push('|');
        for column in 0..columns {
            result.push_str(&format!(
                " {} |",
                row.get(column).map(String::as_str).unwrap_or("")
            ));
        }
        result.push('\n');
        if index == 0 {
            result.push_str(&format!("|{}\n", " --- |".repeat(columns)));
        }
    }
    result + "\n"
}

fn has_clipboard_content(node: &htmd::node::Node<'_>, depth: usize) -> bool {
    if depth > 100 {
        return false;
    }
    if node.text().is_some_and(|s| !s.trim().is_empty()) {
        return true;
    }
    match node.tag_name() {
        Some("script" | "style" | "head" | "iframe" | "object" | "noscript") => return false,
        Some("img" | "imagedata" | "v:imagedata" | "hr") => return true,
        Some("svg") => {
            return node
                .attr("aria-hidden")
                .is_none_or(|v| v.as_ref() != "true")
        }
        Some("a")
            if node
                .attr("aria-label")
                .or_else(|| node.attr("title"))
                .is_some_and(|s| !s.trim().is_empty()) =>
        {
            return true
        }
        _ => {}
    }
    node.children()
        .iter()
        .any(|child| has_clipboard_content(&child, depth + 1))
}

impl Import {
    pub fn prepare(snapshot: Snapshot) -> Self {
        let mut import = Self {
            markdown: String::new(),
            pictures: Vec::new(),
        };
        if let Some(html) = &snapshot.html {
            let pictures = Arc::new(Mutex::new(Vec::new()));
            let captured = pictures.clone();
            let svg_pictures = pictures.clone();
            let linked_pictures = pictures.clone();
            let base = snapshot.source_url.clone();
            let link_base = base.clone();
            let prefix = format!("NMIMAGE{:x}", Sha256::digest(html.as_bytes()));
            let svg_prefix = prefix.clone();
            let converter = HtmlToMarkdown::builder()
                .skip_tags(vec![
                    "script", "style", "head", "iframe", "object", "noscript",
                ])
                .add_handler(vec!["li"], |handlers: &dyn Handlers, element: &Element| {
                    if !has_clipboard_content(element.node, 0) {
                        None
                    } else {
                        handlers.fallback(element)
                    }
                })
                .add_handler(
                    vec!["h1", "h2", "h3", "h4", "h5", "h6"],
                    |handlers: &dyn Handlers, element: &Element| {
                        let body = handlers
                            .walk_children(element.node, element.context)
                            .content;
                        if body.trim().is_empty() {
                            None
                        } else {
                            Some(
                                format!(
                                    "\n\n{} {}\n\n",
                                    "#".repeat(element.tag.as_bytes()[1] as usize - b'0' as usize),
                                    body.split_whitespace().collect::<Vec<_>>().join(" ")
                                )
                                .into(),
                            )
                        }
                    },
                )
                .add_handler(vec!["svg"], move |_: &dyn Handlers, element: &Element| {
                    if element
                        .node
                        .attr("aria-hidden")
                        .is_some_and(|v| v.as_ref() == "true")
                    {
                        return None;
                    }
                    let mut svg = element.node.html();
                    if element.node.attr("xmlns").is_none() {
                        svg = svg.replacen("<svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"", 1);
                    }
                    let alt = element
                        .node
                        .attr("aria-label")
                        .or_else(|| element.node.attr("title"))
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "图标".into());
                    let mut pictures = svg_pictures.lock().unwrap();
                    let token = format!("{svg_prefix}SVG{}END", pictures.len());
                    pictures.push(Picture {
                        token: token.clone(),
                        alt,
                        source: Source::Bytes(svg.into_bytes()),
                        link: None,
                        keep_failed_link: false,
                    });
                    Some(token.into())
                })
                .add_handler(
                    vec!["table"],
                    |handlers: &dyn Handlers, element: &Element| {
                        Some(convert_table(handlers, element.node).into())
                    },
                )
                .add_handler(
                    vec!["span"],
                    |handlers: &dyn Handlers, element: &Element| {
                        let mut body = handlers
                            .walk_children(element.node, element.context)
                            .content;
                        let style = element
                            .node
                            .attr("style")
                            .map(|s| s.to_ascii_lowercase())
                            .unwrap_or_default();
                        for declaration in style.split(';') {
                            let Some((name, value)) = declaration.split_once(':') else {
                                continue;
                            };
                            let value = value.trim();
                            let marker = match name.trim() {
                                "font-weight"
                                    if value == "bold"
                                        || value.parse::<u16>().is_ok_and(|w| w >= 600) =>
                                {
                                    "**"
                                }
                                "font-style" if value == "italic" => "*",
                                "text-decoration" | "text-decoration-line"
                                    if value.contains("line-through") =>
                                {
                                    "~~"
                                }
                                _ => continue,
                            };
                            if !body.trim().is_empty() {
                                body = format!("{marker}{}{marker}", body.trim());
                            }
                        }
                        Some(body.into())
                    },
                )
                .add_handler(
                    vec!["img", "imagedata", "v:imagedata"],
                    move |_: &dyn Handlers, element: &Element| {
                        let src = element
                            .node
                            .attr("src")
                            .or_else(|| element.node.attr("data-src"))
                            .map(|s| s.to_string())
                            .unwrap_or_default();
                        let src = if Url::parse(&src).is_ok() {
                            src
                        } else {
                            base.as_deref()
                                .and_then(external_url)
                                .and_then(|b| b.join(&src).ok())
                                .map(String::from)
                                .unwrap_or(src)
                        };
                        let alt = element
                            .node
                            .attr("alt")
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "图片".into())
                            .replace(['\r', '\n'], " ");
                        let mut pictures = captured.lock().unwrap();
                        let token = format!("{prefix}X{}END", pictures.len());
                        pictures.push(Picture {
                            token: token.clone(),
                            alt,
                            source: Source::Location(src),
                            link: None,
                            keep_failed_link: false,
                        });
                        Some(token.into())
                    },
                )
                .add_handler(
                    vec!["a"],
                    move |handlers: &dyn Handlers, element: &Element| {
                        let mut body = handlers
                            .walk_children(element.node, element.context)
                            .content;
                        if body.trim().is_empty() {
                            body = element
                                .node
                                .attr("aria-label")
                                .or_else(|| element.node.attr("title"))
                                .map(|s| escape(&s))
                                .unwrap_or_default();
                            if body.trim().is_empty() {
                                return None;
                            }
                        }
                        let href = element
                            .node
                            .attr("href")
                            .map(|s| s.to_string())
                            .unwrap_or_default();
                        let url = link_base
                            .as_deref()
                            .and_then(external_url)
                            .and_then(|b| b.join(&href).ok())
                            .or_else(|| Url::parse(&href).ok());
                        Some(
                            if let Some(url) =
                                url.filter(|u| matches!(u.scheme(), "http" | "https" | "mailto"))
                            {
                                let url = url.as_str().replace('>', "%3E");
                                let mut pictures = linked_pictures.lock().unwrap();
                                let mut matches: Vec<_> = pictures
                                    .iter()
                                    .enumerate()
                                    .flat_map(|(i, p)| {
                                        body.match_indices(&p.token)
                                            .map(move |(start, t)| (start, start + t.len(), i))
                                    })
                                    .collect();
                                matches.sort_by_key(|m| m.0);
                                let mut plain = body.clone();
                                for &(_, _, i) in &matches {
                                    plain = plain.replace(&pictures[i].token, "");
                                }
                                let mut output = String::new();
                                let mut cursor = 0;
                                for (start, end, i) in matches {
                                    if start < cursor {
                                        continue;
                                    }
                                    let text = body[cursor..start].trim();
                                    if !text.is_empty() {
                                        output.push_str(&format!("[{text}](<{url}>) "));
                                    }
                                    pictures[i].link = Some(url.clone());
                                    pictures[i].keep_failed_link = plain.trim().is_empty();
                                    output.push_str(&pictures[i].token);
                                    output.push(' ');
                                    cursor = end;
                                }
                                let text = body[cursor..].trim();
                                if !text.is_empty() {
                                    output.push_str(&format!("[{text}](<{url}>)"));
                                }
                                output
                            } else {
                                body
                            }
                            .into(),
                        )
                    },
                )
                .build();
            import.markdown = converter
                .convert(html.as_str())
                .unwrap_or_else(|_| snapshot.text.clone().unwrap_or_default());
            import.pictures = std::mem::take(&mut *pictures.lock().unwrap());
            import
                .pictures
                .retain(|p| import.markdown.contains(&p.token));
            // Word puts local image references in HTML and embeds their bytes in RTF.
            let embedded = snapshot.rtf.as_deref().map(rtf_images).unwrap_or_default();
            let mut local_sources = Vec::new();
            for picture in &import.pictures {
                if let Source::Location(src) = &picture.source {
                    if (src.starts_with("file:") || src.starts_with("cid:"))
                        && !local_sources.contains(src)
                    {
                        local_sources.push(src.clone());
                    }
                }
            }
            if local_sources.len() == embedded.len() {
                for picture in &mut import.pictures {
                    if let Source::Location(src) = &picture.source {
                        if let Some(index) = local_sources.iter().position(|s| s == src) {
                            picture.source = Source::Bytes(embedded[index].clone());
                        }
                    }
                }
            } else if import.pictures.len() == 1
                && snapshot.images.len() == 1
                && matches!(&import.pictures[0].source, Source::Location(s) if s.starts_with("file:") || s.starts_with("cid:"))
            {
                import.pictures[0].source = Source::Bytes(snapshot.images[0].clone());
            }
        } else {
            for bytes in snapshot.images {
                let token = import.picture("图片", Source::Bytes(bytes));
                import.markdown.push_str(&format!("{token}\n\n"));
            }
            for path in snapshot.files.into_iter().filter(|p| supported_path(p)) {
                let alt = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                if let Ok(url) = Url::from_file_path(path) {
                    let token = import.picture(&alt, Source::Location(url.into()));
                    import.markdown.push_str(&format!("{token}\n\n"));
                }
            }
        }
        if import.markdown.is_empty() {
            import.markdown = snapshot.text.unwrap_or_default();
        }
        import
    }

    fn picture(&mut self, alt: &str, source: Source) -> String {
        let token = format!("NATIVEIMAGEPLACEHOLDER{}END", self.pictures.len());
        self.pictures.push(Picture {
            token: token.clone(),
            alt: alt.replace(['\r', '\n'], " "),
            source,
            link: None,
            keep_failed_link: false,
        });
        token
    }
    pub fn has_images(&self) -> bool {
        !self.pictures.is_empty()
    }

    pub fn finish(
        self,
        path: &Path,
        cancelled: Arc<AtomicBool>,
        timeouts: DownloadTimeouts,
    ) -> Finished {
        let deadline = Instant::now() + Duration::from_secs(timeouts.batch_secs);
        let shared = Arc::new(Mutex::new((
            0usize,
            0usize,
            HashMap::<String, String>::new(),
        )));
        let results = Mutex::new(Vec::new());
        let client = reqwest::blocking::Client::builder()
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(timeouts.connect_secs))
            .timeout(Duration::from_secs(timeouts.image_secs))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 || external_url(attempt.url().as_str()).is_none() {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .build();
        let client = match client {
            Ok(client) => client,
            Err(_) => return self.fail_all("网络初始化失败"),
        };
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let shared = &shared;
                let results = &results;
                let client = &client;
                let pictures = &self.pictures;
                let cancelled = &cancelled;
                scope.spawn(move || loop {
                    let index = {
                        let mut state = shared.lock().unwrap();
                        let i = state.0;
                        state.0 += 1;
                        i
                    };
                    let Some(picture) = pictures.get(index) else {
                        break;
                    };
                    let result = if index >= MAX_IMAGES {
                        Err("超过每次50张限制".into())
                    } else if cancelled.load(Ordering::Acquire) {
                        Err("已取消".into())
                    } else {
                        load(
                            &picture.source,
                            client,
                            deadline,
                            cancelled,
                            Duration::from_secs(timeouts.image_secs),
                        )
                        .and_then(|bytes| {
                            let mut state = shared.lock().unwrap();
                            let hash = format!("{:x}", Sha256::digest(&bytes));
                            let is_new = !state.2.contains_key(&hash);
                            if is_new && state.1 + bytes.len() > MAX_TOTAL {
                                return Err("超过每次100 MiB限制".into());
                            }
                            if cancelled.load(Ordering::Acquire) {
                                return Err("已取消".into());
                            }
                            let relative = store(path, &bytes, &mut state.2)
                                .map_err(|_| "附件写入失败".to_owned())?;
                            if is_new {
                                state.1 += bytes.len();
                            }
                            Ok(relative)
                        })
                    };
                    results.lock().unwrap().push((index, result));
                });
            }
        });
        let mut markdown = self.markdown;
        let mut failed = 0;
        for (index, result) in results.into_inner().unwrap() {
            let picture = &self.pictures[index];
            let replacement = match result {
                Ok(relative) => {
                    let image =
                        format!("![{}](<{}>)", escape(&picture.alt), destination(&relative));
                    picture
                        .link
                        .as_ref()
                        .map_or_else(|| image.clone(), |url| format!("[{image}](<{url}>)"))
                }
                Err(reason) => {
                    failed += 1;
                    failure(picture, &reason)
                }
            };
            markdown = markdown.replace(&picture.token, &replacement);
        }
        Finished { markdown, failed }
    }
    fn fail_all(self, reason: &str) -> Finished {
        let mut markdown = self.markdown;
        for picture in &self.pictures {
            markdown = markdown.replace(&picture.token, &failure(picture, reason));
        }
        Finished {
            markdown,
            failed: self.pictures.len(),
        }
    }
}

pub struct Finished {
    pub markdown: String,
    pub failed: usize,
}

/// Keep an asynchronous insertion attached to the original selection while edits
/// elsewhere continue. An overlapping edit invalidates the pending insertion.
pub fn rebase_selection(range: &mut std::ops::Range<usize>, before: &str, after: &str) -> bool {
    let prefix = before
        .chars()
        .zip(after.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let suffix = before[prefix..]
        .chars()
        .rev()
        .zip(after[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let end = before.len() - suffix;
    let inserted = after.len() - suffix - prefix;
    if end <= range.start {
        let shift = inserted as isize - (end - prefix) as isize;
        range.start = range.start.saturating_add_signed(shift);
        range.end = range.end.saturating_add_signed(shift);
        true
    } else {
        prefix >= range.end
    }
}
fn failure(picture: &Picture, reason: &str) -> String {
    let label = format!("图片下载失败：{}（{reason}）", picture.alt);
    if let Source::Location(src) = &picture.source {
        if let Some(url) = external_url(src) {
            let failure = format!(
                "[{}](<{}>)",
                escape(&label),
                url.as_str().replace('>', "%3E")
            );
            return append_failed_link(picture, failure);
        }
    }
    append_failed_link(picture, format!("[{}]", escape(&label)))
}
fn append_failed_link(picture: &Picture, text: String) -> String {
    if picture.keep_failed_link {
        if let Some(url) = &picture.link {
            return format!("{text} [原链接](<{url}>)");
        }
    }
    text
}
fn supported_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        matches!(
            e.to_string_lossy().to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg"
        )
    })
}
fn extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("webp")
    } else if bytes.starts_with(b"BM") {
        Some("bmp")
    } else if std::str::from_utf8(bytes).is_ok_and(|s| {
        let s = s.trim_start_matches('\u{feff}').trim_start();
        s.starts_with("<svg") || (s.starts_with("<?xml") && s.contains("<svg"))
    }) {
        Some("svg")
    } else {
        None
    }
}
fn bounded_read(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_IMAGE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "读取图片失败或超时".to_owned())?;
    if bytes.len() > MAX_IMAGE {
        return Err("超过单张20 MiB限制".into());
    }
    Ok(bytes)
}
fn load(
    source: &Source,
    client: &reqwest::blocking::Client,
    deadline: Instant,
    cancel: &AtomicBool,
    image_timeout: Duration,
) -> Result<Vec<u8>, String> {
    let bytes = match source {
        Source::Bytes(bytes) => {
            if bytes.len() > MAX_IMAGE {
                return Err("超过单张20 MiB限制".into());
            }
            bytes.clone()
        }
        Source::Location(src) if src.starts_with("data:") => {
            let (header, data) = src.split_once(',').ok_or("无效内嵌图片")?;
            if data.len() > MAX_IMAGE * 4 / 3 + 8 {
                return Err("超过单张20 MiB限制".into());
            }
            if header.ends_with(";base64") {
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|_| "无效内嵌图片")?
            } else if header.starts_with("data:image/svg+xml") {
                percent_decode(data).into_bytes()
            } else {
                return Err("不支持的内嵌图片编码".into());
            }
        }
        Source::Location(src) => {
            let url = Url::parse(src).map_err(|_| "缺少可用图片地址")?;
            if url.scheme() == "file" {
                let path = url.to_file_path().map_err(|_| "无效本地图片路径")?;
                // Never read UNC paths as a side effect of pasting HTML (network credentials).
                if url
                    .host_str()
                    .is_some_and(|h| !h.is_empty() && h != "localhost")
                {
                    return Err("不支持网络共享图片".into());
                }
                bounded_read(fs::File::open(path).map_err(|_| "本地图片不可用")?)?
            } else {
                let url = external_url(src).ok_or("不支持或需要应用授权的图片地址")?;
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() || cancel.load(Ordering::Acquire) {
                    return Err("下载超时或已取消".into());
                }
                let response = client
                    .get(url)
                    .timeout(remaining.min(image_timeout))
                    .send()
                    .map_err(|e| {
                        if e.is_timeout() && e.is_connect() {
                            "连接超时"
                        } else if e.is_timeout() {
                            "下载超时"
                        } else {
                            "下载失败"
                        }
                    })?;
                let response = response
                    .error_for_status()
                    .map_err(|_| "服务器拒绝或图片不存在")?;
                if response
                    .content_length()
                    .is_some_and(|n| n > MAX_IMAGE as u64)
                {
                    return Err("超过单张20 MiB限制".into());
                }
                bounded_read(response)?
            }
        }
    };
    if extension(&bytes).is_none() {
        return Err("不支持或无效的图片格式".into());
    }
    Ok(bytes)
}

pub fn asset_directory(path: &Path) -> String {
    format!(
        "{}.assets",
        path.file_stem().unwrap_or_default().to_string_lossy()
    )
}

/// Only copy referenced imports, never scan an attachment directory. Preserve relative
/// names on Save As so editor text, selections and its undo history stay intact.
#[derive(Default)]
pub struct Migration {
    created: Vec<PathBuf>,
    directories: Vec<PathBuf>,
}
impl Migration {
    pub fn prepare(content: &str, old: &Path, new: &Path) -> std::io::Result<Self> {
        use pulldown_cmark::{Event, Parser, Tag};
        let mut migration = Self::default();
        let source = old.parent().unwrap_or_else(|| Path::new("."));
        let target = new.parent().unwrap_or_else(|| Path::new("."));
        if source == target {
            return Ok(migration);
        }
        let mut seen = std::collections::HashSet::new();
        for event in Parser::new(content) {
            let Event::Start(Tag::Image { dest_url, .. }) = event else {
                continue;
            };
            let decoded = percent_decode(&dest_url);
            let parts: Vec<_> = decoded.split('/').collect();
            if parts.len() != 2
                || !parts[0].ends_with(".assets")
                || parts[0].contains(['\\', ':'])
                || !parts[1].starts_with("nm-")
                || parts[1].contains(['\\', ':'])
                || !seen.insert(decoded.clone())
            {
                continue;
            }
            let Some((hash, _)) = parts[1].strip_prefix("nm-").and_then(|s| s.split_once('.'))
            else {
                continue;
            };
            if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let original = source.join(&decoded);
            if !original.canonicalize()?.starts_with(source.canonicalize()?) {
                return Err(std::io::Error::other(
                    "attachment escapes original document root",
                ));
            }
            let bytes = bounded_read(fs::File::open(original)?).map_err(std::io::Error::other)?;
            if format!("{:x}", Sha256::digest(&bytes)) != hash {
                return Err(std::io::Error::other("imported attachment was changed"));
            }
            let folder = target.join(parts[0]);
            if !folder.exists() {
                fs::create_dir(&folder)?;
                migration.directories.push(folder.clone());
            }
            if !folder.canonicalize()?.starts_with(target.canonicalize()?) {
                return Err(std::io::Error::other("attachment escapes destination root"));
            }
            let destination = target.join(&decoded);
            if destination.exists() {
                if !destination
                    .canonicalize()?
                    .starts_with(target.canonicalize()?)
                    || fs::read(&destination)? != bytes
                {
                    return Err(std::io::Error::other(
                        "destination attachment already has different content",
                    ));
                }
            } else {
                let mut temp = tempfile::NamedTempFile::new_in(&folder)?;
                temp.write_all(&bytes)?;
                temp.persist_noclobber(&destination).map_err(|e| e.error)?;
                migration.created.push(destination);
            }
        }
        Ok(migration)
    }
    pub fn commit(mut self) {
        self.created.clear();
        self.directories.clear();
    }
}
impl Drop for Migration {
    fn drop(&mut self) {
        for file in self.created.iter().rev() {
            let _ = fs::remove_file(file);
        }
        for directory in self.directories.iter().rev() {
            let _ = fs::remove_dir(directory);
        }
    }
}
fn percent_decode(value: &str) -> String {
    let mut bytes = Vec::new();
    let value = value.as_bytes();
    let mut i = 0;
    while i < value.len() {
        if value[i] == b'%' && i + 2 < value.len() {
            if let Ok(hex) = std::str::from_utf8(&value[i + 1..i + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    bytes.push(byte);
                    i += 3;
                    continue;
                }
            }
        }
        bytes.push(value[i]);
        i += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
fn store(
    path: &Path,
    bytes: &[u8],
    cache: &mut HashMap<String, String>,
) -> std::io::Result<String> {
    let hash = format!("{:x}", Sha256::digest(bytes));
    if let Some(relative) = cache.get(&hash) {
        return Ok(relative.clone());
    }
    let filename = format!("nm-{hash}.{}", extension(bytes).unwrap());
    let directory = asset_directory(path);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let folder = parent.join(&directory);
    fs::create_dir_all(&folder)?;
    if !folder.canonicalize()?.starts_with(parent.canonicalize()?) {
        return Err(std::io::Error::other(
            "attachment directory escapes document root",
        ));
    }
    let target = folder.join(filename);
    if !target.exists() {
        let mut temp = tempfile::NamedTempFile::new_in(&folder)?;
        temp.write_all(bytes)?;
        temp.persist_noclobber(&target).map_err(|e| e.error)?;
    } else if !target.canonicalize()?.starts_with(parent.canonicalize()?)
        || fs::read(&target)? != bytes
    {
        return Err(std::io::Error::other(
            "attachment name collision or escaping symlink",
        ));
    }
    let relative = format!(
        "{directory}/{}",
        target.file_name().unwrap().to_string_lossy()
    );
    cache.insert(hash, relative.clone());
    Ok(relative)
}

fn rtf_images(rtf: &str) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while let Some(start) = rtf[cursor..].find("{\\pict") {
        let start = cursor + start;
        let mut depth = 0i32;
        let mut end = None;
        for (i, b) in rtf.as_bytes()[start..].iter().enumerate() {
            if *b == b'{' {
                depth += 1;
            }
            if *b == b'}' {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i);
                    break;
                }
            }
        }
        let Some(end) = end else {
            break;
        };
        cursor = end + 1;
        let group = &rtf[start..end];
        if !group.contains("\\pngblip") && !group.contains("\\jpegblip") {
            continue;
        }
        let mut hex = String::new();
        let bytes = group.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if i > 0 && bytes[i] == b'{' {
                let mut nested = 1;
                i += 1;
                while i < bytes.len() && nested > 0 {
                    if bytes[i] == b'{' {
                        nested += 1;
                    } else if bytes[i] == b'}' {
                        nested -= 1;
                    }
                    i += 1;
                }
                continue;
            }
            if bytes[i] == b'\\' {
                i += 1;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-') {
                    i += 1;
                }
                if i < bytes.len() && bytes[i] == b' ' {
                    i += 1;
                }
            } else {
                if bytes[i].is_ascii_hexdigit() {
                    hex.push(bytes[i] as char);
                }
                i += 1;
            }
        }
        if hex.len() > MAX_IMAGE * 2 {
            continue;
        }
        let image: Option<Vec<u8>> = (0..hex.len())
            .step_by(2)
            .map(|i| {
                hex.get(i..i + 2)
                    .and_then(|s| u8::from_str_radix(s, 16).ok())
            })
            .collect();
        if let Some(image) = image.filter(|b| extension(b).is_some()) {
            result.push(image);
        }
        if result.len() >= MAX_IMAGES {
            break;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn png() -> Vec<u8> {
        base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap()
    }
    #[test]
    fn linked_image_failure_is_not_a_nested_markdown_link() {
        let import=Import::prepare(html("<a href='https://huggingface.co/'><img alt='Hugging Face logo' src='https://huggingface.co/logo.svg'><strong>Hugging Face</strong></a>"));
        let result = import.fail_all("下载超时");
        assert!(!result.markdown.starts_with("[["), "{}", result.markdown);
        let links = pulldown_cmark::Parser::new(&result.markdown)
            .filter(|e| {
                matches!(
                    e,
                    pulldown_cmark::Event::Start(pulldown_cmark::Tag::Link { .. })
                )
            })
            .count();
        assert_eq!(
            links, 2,
            "logo source and surrounding page must both remain usable: {}",
            result.markdown
        );
    }
    #[test]
    fn inline_svg_logo_is_imported_instead_of_becoming_an_empty_link() {
        let import=Import::prepare(html("<h1><a href='https://huggingface.co/Qwen'><svg viewBox='0 0 24 24' aria-label='Qwen logo'><path d='M0 0h24v24H0z'/></svg></a></h1>"));
        assert!(import.has_images(), "{}", import.markdown);
        assert!(!import.markdown.contains("[]("), "{}", import.markdown);
        let directory = tempfile::tempdir().unwrap();
        let result = import.finish(
            &directory.path().join("svg.md"),
            Arc::new(AtomicBool::new(false)),
            DownloadTimeouts::default(),
        );
        assert_eq!(result.failed, 0);
        assert!(result.markdown.contains("svg.assets/nm-"));
        let file = fs::read_dir(directory.path().join("svg.assets"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(fs::read_to_string(file)
            .unwrap()
            .contains("xmlns=\"http://www.w3.org/2000/svg\""));
    }
    #[test]
    fn empty_browser_links_and_nested_empty_menu_items_are_omitted() {
        let result=Import::prepare(html("<h1><a href='https://example.com'><div></div></a><div>Qwen</div><div>/</div><a href='https://example.com/model'>Model</a></h1><ul><li>More<ul><li><ul><li><a href='https://example.com/hidden'></a></li></ul></li></ul></li><li><a href='https://example.com/a' aria-label='Available'></a></li></ul>"));
        assert!(!result.markdown.contains("[]("));
        assert!(!result.markdown.contains("/hidden"));
        assert!(
            result.markdown.contains("# Qwen / [Model]"),
            "{}",
            result.markdown
        );
        assert!(result.markdown.contains("[Available]"));
    }
    #[test]
    #[ignore = "explicit real-clipboard capture and live network diagnostic"]
    fn replay_captured_clipboard() {
        let capture = std::env::var("NATIVE_MARKDOWN_PASTE_REPLAY")
            .expect("path to authorized CF_HTML capture");
        let raw = fs::read_to_string(&capture).unwrap();
        let (fragment, source_url) = html_fragment(&raw);
        let parent = Path::new(&capture).parent().unwrap();
        let folder = tempfile::Builder::new()
            .prefix("replay-")
            .tempdir_in(parent)
            .unwrap()
            .keep();
        let start = Instant::now();
        let result = finish(
            Snapshot {
                html: Some(fragment),
                source_url,
                ..Default::default()
            },
            &folder.join("replay.md"),
        );
        fs::write(folder.join("replay.md"), &result.markdown).unwrap();
        println!(
            "replay: failed_images={} elapsed_ms={} output={}",
            result.failed,
            start.elapsed().as_millis(),
            folder.join("replay.md").display()
        );
        assert_eq!(result.failed, 0, "{}", result.markdown);
        assert!(!result.markdown.contains("[]("));
    }
    fn html(value: &str) -> Snapshot {
        Snapshot {
            html: Some(value.into()),
            ..Default::default()
        }
    }
    fn finish(snapshot: Snapshot, path: &Path) -> Finished {
        Import::prepare(snapshot).finish(
            path,
            Arc::new(AtomicBool::new(false)),
            DownloadTimeouts::default(),
        )
    }
    #[test]
    fn cf_html_uses_byte_offsets_and_marker_fallback() {
        let header="Version:1.0\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\nSourceURL:https://example.com/page\r\n";
        let body = "<p>中文🦀</p>";
        let raw = format!("{header}{body}")
            .replacen(
                "StartFragment:0000000000",
                &format!("StartFragment:{:010}", header.len()),
                1,
            )
            .replacen(
                "EndFragment:0000000000",
                &format!("EndFragment:{:010}", header.len() + body.len()),
                1,
            );
        assert_eq!(
            html_fragment(&raw),
            (body.into(), Some("https://example.com/page".into()))
        );
        assert_eq!(
            html_fragment("Version:1\n<!--StartFragment--><b>你好</b><!--EndFragment-->").0,
            "<b>你好</b>"
        );
    }
    #[test]
    fn rich_text_preserves_semantics_and_omits_executable_content() {
        let snapshot=html("<h1>标题</h1><p><b>粗体</b> <i>斜体</i><a href='https://example.com'>链接</a></p><ul><li>一<ul><li>二</li></ul></li></ul><blockquote>引用</blockquote><pre><code>a &lt; b</code></pre><table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table><script>bad()</script>");
        let import = Import::prepare(snapshot);
        assert!(import.markdown.contains("# 标题"));
        assert!(import.markdown.contains("**粗体**"));
        assert!(import.markdown.contains("*斜体*"));
        assert!(import.markdown.contains("https://example.com"));
        assert!(import.markdown.contains("引用"));
        assert!(import.markdown.contains("a < b"));
        assert!(import.markdown.contains('|'));
        assert!(!import.markdown.contains("bad()"));
    }
    #[test]
    fn embedded_and_repeated_images_use_one_local_file() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("笔 记.md");
        let data = base64::engine::general_purpose::STANDARD.encode(png());
        let result=finish(html(&format!("<p>之前<img width='100' src='data:image/png;base64,{data}' alt='示意图'>之后<img src='data:image/png;base64,{data}'></p>")),&path);
        assert_eq!(result.failed, 0);
        assert!(result.markdown.contains("笔%20记.assets/"));
        assert!(!result.markdown.contains("PLACEHOLDER"));
        assert_eq!(
            fs::read_dir(folder.path().join("笔 记.assets"))
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn word_local_image_uses_rtf_embedded_bytes() {
        let mut snapshot = html("<p>Word<img src='file:///C:/missing/word-image.png'></p>");
        snapshot.rtf = Some(format!(
            "{{\\rtf1 {{\\pict\\pngblip {} }} }}",
            png().iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        let folder = tempfile::tempdir().unwrap();
        let result = finish(snapshot, &folder.path().join("word.md"));
        assert_eq!(result.failed, 0);
        assert!(result.markdown.contains("word.assets/"));
    }
    #[test]
    fn bad_remote_image_preserves_source_and_reason() {
        let folder = tempfile::tempdir().unwrap();
        let result = finish(
            html("<img src='https://127.0.0.1:1/no.png' alt='图[1]'>"),
            &folder.path().join("a.md"),
        );
        assert_eq!(result.failed, 1);
        assert!(result.markdown.contains("图片下载失败"));
        assert!(result.markdown.contains("https://127.0.0.1:1/no.png"));
        assert!(!folder.path().join("a.assets").exists());
    }
    #[test]
    fn cancelled_import_does_not_create_files() {
        let folder = tempfile::tempdir().unwrap();
        let snapshot = Snapshot {
            images: vec![png()],
            ..Default::default()
        };
        let result = Import::prepare(snapshot).finish(
            &folder.path().join("a.md"),
            Arc::new(AtomicBool::new(true)),
            DownloadTimeouts::default(),
        );
        assert_eq!(result.failed, 1);
        assert!(!folder.path().join("a.assets").exists());
    }
    #[test]
    fn invalid_image_and_oversize_are_rejected_without_writes() {
        let folder = tempfile::tempdir().unwrap();
        let snapshot = Snapshot {
            images: vec![b"<html>login</html>".to_vec(), vec![0; MAX_IMAGE + 1]],
            ..Default::default()
        };
        let result = finish(snapshot, &folder.path().join("a.md"));
        assert_eq!(result.failed, 2);
        assert!(!folder.path().join("a.assets").exists());
    }
    #[test]
    fn save_as_copies_only_referenced_imports_and_rolls_back_conflicts() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let path = source.path().join("a.md");
        let result = finish(
            Snapshot {
                images: vec![png()],
                ..Default::default()
            },
            &path,
        );
        fs::write(source.path().join("a.assets/unrelated.txt"), "keep").unwrap();
        let migration =
            Migration::prepare(&result.markdown, &path, &target.path().join("b.md")).unwrap();
        migration.commit();
        assert_eq!(
            fs::read_dir(target.path().join("a.assets"))
                .unwrap()
                .count(),
            1
        );
        let file = fs::read_dir(target.path().join("a.assets"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        fs::write(&file, "different").unwrap();
        assert!(Migration::prepare(&result.markdown, &path, &target.path().join("b.md")).is_err());
        assert_eq!(fs::read_to_string(file).unwrap(), "different");
    }
    #[test]
    fn markdown_save_failure_rolls_back_new_attachment_copies() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let original = source.path().join("a.md");
        fs::write(&original, "original").unwrap();
        let result = finish(
            Snapshot {
                images: vec![png()],
                ..Default::default()
            },
            &original,
        );
        let mut document = crate::document::Document::open(original.clone()).unwrap();
        document.content = result.markdown;
        let invalid = target.path().join("cannot-replace-directory.md");
        fs::create_dir(&invalid).unwrap();
        assert!(document.save_as(invalid).is_err());
        assert_eq!(document.path, Some(original.clone()));
        assert!(document.is_dirty());
        assert_eq!(fs::read_to_string(original).unwrap(), "original");
        assert!(!target.path().join("a.assets").exists());
    }
    #[test]
    fn relative_web_image_resolves_against_source_url() {
        let mut snapshot = html("<img src='/images/a.png'>");
        snapshot.source_url = Some("https://example.com/notes/page".into());
        let import = Import::prepare(snapshot);
        assert!(
            matches!(&import.pictures[0].source,Source::Location(s) if s=="https://example.com/images/a.png")
        );
    }
    #[test]
    fn http_download_sniffs_bytes_and_rejects_login_pages() {
        use std::net::TcpListener;
        for (body, success) in [(png(), true), (b"<html>Sign in</html>".to_vec(), false)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let length = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..length]).to_lowercase();
                assert!(!request.contains("cookie:"));
                assert!(!request.contains("authorization:"));
                write!(stream,"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).unwrap();
                stream.write_all(&body).unwrap();
            });
            let folder = tempfile::tempdir().unwrap();
            let result = finish(
                html(&format!("<img src='http://{address}/image'>")),
                &folder.path().join("a.md"),
            );
            server.join().unwrap();
            assert_eq!(result.failed, usize::from(!success));
        }
    }
    #[test]
    fn slow_body_obeys_entire_request_deadline() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n")
                .unwrap();
            std::thread::sleep(Duration::from_millis(300));
        });
        let client = reqwest::blocking::Client::builder()
            .no_proxy()
            .build()
            .unwrap();
        let cancel = AtomicBool::new(false);
        let start = Instant::now();
        let result = load(
            &Source::Location(format!("http://{address}/slow")),
            &client,
            start + Duration::from_millis(80),
            &cancel,
            Duration::from_secs(10),
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_millis(250));
        server.join().unwrap();
    }
    #[test]
    fn configured_image_and_batch_timeouts_reach_downloads() {
        use std::net::TcpListener;
        for (image_secs, batch_secs) in [(1, 5), (5, 1)] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n",
                );
                std::thread::sleep(Duration::from_millis(1800));
            });
            let folder = tempfile::tempdir().unwrap();
            let import = Import::prepare(html(&format!(
                "<img src='http://{address}/slow' alt='slow'>"
            )));
            let start = Instant::now();
            let result = import.finish(
                &folder.path().join("slow.md"),
                Arc::new(AtomicBool::new(false)),
                DownloadTimeouts {
                    connect_secs: 5,
                    image_secs,
                    batch_secs,
                },
            );
            assert!(start.elapsed() < Duration::from_millis(1600));
            assert_eq!(result.failed, 1);
            assert!(result.markdown.contains("图片下载失败"));
            assert!(result.markdown.contains(&format!("http://{address}/slow")));
            assert!(!folder.path().join("slow.assets").exists());
            server.join().unwrap();
        }
    }

    #[test]
    fn image_count_limit_keeps_failure_locations() {
        let folder = tempfile::tempdir().unwrap();
        let result = finish(
            Snapshot {
                images: (0..51).map(|_| png()).collect(),
                ..Default::default()
            },
            &folder.path().join("a.md"),
        );
        assert_eq!(result.failed, 1);
        assert!(result.markdown.contains("超过每次50张限制"));
        assert_eq!(
            fs::read_dir(folder.path().join("a.assets"))
                .unwrap()
                .count(),
            1
        );
    }
    #[test]
    fn pending_selection_tracks_unicode_edits_and_rejects_overlap() {
        let mut range = 4..7;
        assert!(rebase_selection(&mut range, "🦀abc", "你好🦀abc"));
        assert_eq!(range, 10..13);
        assert!(!rebase_selection(&mut range, "你好🦀abc", "你好🦀aXc"));
        let mut range = 4..4;
        assert!(rebase_selection(&mut range, "🦀abc", "🦀a🦀bc"));
        assert_eq!(range, 4..4);
    }
}
