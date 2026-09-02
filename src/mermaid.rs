use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::ops::Range;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use futures::channel::oneshot;
use merman::render::{HeadlessRenderer, HostThemeOutput, HostThemeProfile, HostThemeRoles};
use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};

use crate::markdown;

pub const URI_PREFIX: &str = "native-markdown-mermaid://v1/";
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_DOCUMENT_DIAGRAMS: usize = 64;
pub const MAX_SVG_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_SVG_ELEMENTS: usize = 50_000;
pub const MAX_RASTER_WIDTH: u32 = 1_280;
pub const MAX_RASTER_PIXELS: u64 = 4_194_304;
pub const EDIT_TIMEOUT: Duration = Duration::from_secs(1);
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(3);

const WORKER_ARG: &str = "--native-markdown-mermaid-worker";
const SELF_TEST_ARG: &str = "--native-markdown-mermaid-self-test";
const MAX_ERROR_BYTES: usize = 16 * 1024;
const RENDERER_CACHE_VERSION: &str = "merman-0.7.0/native-markdown-theme-v1";
static WORKER_PID: AtomicU64 = AtomicU64::new(0);

pub fn worker_pid() -> Option<u32> {
    u32::try_from(WORKER_PID.load(Ordering::Relaxed))
        .ok()
        .filter(|pid| *pid != 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportLevel {
    Supported,
    Experimental,
    Unsupported,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidBlock {
    pub index: usize,
    pub whole_range: Range<usize>,
    pub body_range: Range<usize>,
    pub source: String,
    pub diagram_label: String,
    pub support: SupportLevel,
    source_key: String,
    anchor_key: String,
}

#[derive(Clone, Debug)]
enum RenderStatus {
    Loading {
        last_good: Option<String>,
    },
    Ready {
        asset_key: String,
    },
    Error {
        message: String,
        last_good: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MermaidPreviewStatus {
    Ready {
        image_uri: String,
    },
    Loading {
        image_uri: Option<String>,
        message: String,
    },
    Error {
        image_uri: Option<String>,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidPreview {
    pub index: usize,
    pub diagram_label: String,
    pub experimental: bool,
    pub display_width: Option<u32>,
    pub status: MermaidPreviewStatus,
}

#[derive(Clone, Debug)]
struct BlockState {
    block: MermaidBlock,
    status: RenderStatus,
}

#[derive(Clone, Debug)]
pub struct RenderJob {
    pub source_key: String,
    pub source: String,
    pub timeout: Duration,
}

pub struct MermaidManager {
    worker: MermaidWorker,
    blocks: Vec<BlockState>,
    ready_keys: HashSet<String>,
    asset_widths: HashMap<String, f32>,
    pending_keys: HashSet<String>,
}

impl MermaidManager {
    pub fn new() -> Self {
        Self {
            worker: MermaidWorker::new(),
            blocks: Vec::new(),
            ready_keys: HashSet::new(),
            asset_widths: HashMap::new(),
            pending_keys: HashSet::new(),
        }
    }

    pub fn reset(&mut self) {
        self.blocks.clear();
        self.ready_keys.clear();
        self.asset_widths.clear();
        self.pending_keys.clear();
    }

    pub fn refresh(&mut self, source: &str, timeout: Duration) -> Vec<RenderJob> {
        let discovered = discover_blocks(source);
        let old_by_anchor = self
            .blocks
            .drain(..)
            .map(|state| (state.block.anchor_key.clone(), state))
            .collect::<HashMap<_, _>>();
        let mut jobs = Vec::new();

        self.blocks = discovered
            .into_iter()
            .map(|block| {
                let last_good = old_by_anchor.get(&block.anchor_key).and_then(|old| {
                    match &old.status {
                        RenderStatus::Ready { asset_key } => Some(asset_key.clone()),
                        RenderStatus::Loading { last_good }
                        | RenderStatus::Error { last_good, .. } => last_good.clone(),
                    }
                });

                let status = if block.index >= MAX_DOCUMENT_DIAGRAMS {
                    RenderStatus::Error {
                        message: format!(
                            "This document exceeds the limit of {MAX_DOCUMENT_DIAGRAMS} Mermaid diagrams"
                        ),
                        last_good,
                    }
                } else if let Err(error) = validate_source(&block.source, block.support) {
                    RenderStatus::Error {
                        message: error,
                        last_good,
                    }
                } else if self.ready_keys.contains(&block.source_key) {
                    RenderStatus::Ready {
                        asset_key: block.source_key.clone(),
                    }
                } else {
                    if self.pending_keys.insert(block.source_key.clone()) {
                        jobs.push(RenderJob {
                            source_key: block.source_key.clone(),
                            source: block.source.clone(),
                            timeout,
                        });
                    }
                    RenderStatus::Loading { last_good }
                };

                BlockState { block, status }
            })
            .collect();

        let active_keys = self
            .blocks
            .iter()
            .map(|state| state.block.source_key.clone())
            .collect::<HashSet<_>>();
        self.pending_keys.retain(|key| active_keys.contains(key));
        let referenced_assets = self.referenced_assets();
        self.asset_widths
            .retain(|key, _| referenced_assets.contains(key));
        jobs
    }

    pub fn worker(&self) -> MermaidWorker {
        self.worker.clone()
    }

    pub fn needs_result(&self, source_key: &str) -> bool {
        self.blocks
            .iter()
            .any(|state| state.block.source_key == source_key)
    }

    pub fn cancel_pending(&mut self, source_key: &str) {
        self.pending_keys.remove(source_key);
    }

    pub fn apply_result(
        &mut self,
        source_key: &str,
        result: Result<String, String>,
    ) -> Option<Arc<[u8]>> {
        self.pending_keys.remove(source_key);
        if !self.needs_result(source_key) {
            return None;
        }

        match result {
            Ok(svg) => {
                let intrinsic_width = svg_intrinsic_width(svg.as_bytes());
                let svg: Arc<[u8]> = svg.into_bytes().into();
                self.ready_keys.insert(source_key.to_owned());
                if let Some(intrinsic_width) = intrinsic_width {
                    self.asset_widths
                        .insert(source_key.to_owned(), intrinsic_width);
                }
                for state in &mut self.blocks {
                    if state.block.source_key == source_key {
                        state.status = RenderStatus::Ready {
                            asset_key: source_key.to_owned(),
                        };
                    }
                }
                Some(svg)
            }
            Err(message) => {
                self.ready_keys.remove(source_key);
                for state in &mut self.blocks {
                    if state.block.source_key == source_key {
                        let last_good = match &state.status {
                            RenderStatus::Loading { last_good }
                            | RenderStatus::Error { last_good, .. } => last_good.clone(),
                            RenderStatus::Ready { asset_key } => Some(asset_key.clone()),
                        };
                        state.status = RenderStatus::Error {
                            message: clean_error(&message),
                            last_good,
                        };
                    }
                }
                None
            }
        }
    }

    pub fn referenced_assets(&self) -> HashSet<String> {
        let mut keys = HashSet::new();
        for state in &self.blocks {
            match &state.status {
                RenderStatus::Ready { asset_key } => {
                    keys.insert(asset_key.clone());
                }
                RenderStatus::Loading {
                    last_good: Some(asset_key),
                }
                | RenderStatus::Error {
                    last_good: Some(asset_key),
                    ..
                } => {
                    keys.insert(asset_key.clone());
                }
                _ => {}
            }
        }
        keys
    }

    pub fn error_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|state| matches!(state.status, RenderStatus::Error { .. }))
            .count()
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    pub fn ready_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|state| matches!(state.status, RenderStatus::Ready { .. }))
            .count()
    }

    pub fn pending_count(&self) -> usize {
        self.pending_keys.len()
    }

    pub fn block_body_range(&self, index: usize) -> Option<Range<usize>> {
        self.blocks
            .iter()
            .find(|state| state.block.index == index)
            .map(|state| state.block.body_range.clone())
    }

    pub fn preview_for_block(
        &self,
        source_range: Range<usize>,
        viewport_width: u32,
        zoom_percent: u32,
    ) -> Option<MermaidPreview> {
        let state = self
            .blocks
            .iter()
            .find(|state| state.block.whole_range == source_range)?;
        let image_uri = |asset_key: &str| format!("{URI_PREFIX}{asset_key}.png");
        let asset_key = match &state.status {
            RenderStatus::Ready { asset_key } => Some(asset_key.as_str()),
            RenderStatus::Loading { last_good } | RenderStatus::Error { last_good, .. } => {
                last_good.as_deref()
            }
        };
        let display_width = asset_key
            .and_then(|key| self.asset_widths.get(key))
            .map(|width| {
                (width * zoom_percent.clamp(50, 250) as f32 / 100.0)
                    .ceil()
                    .clamp(1.0, viewport_width.clamp(1, MAX_RASTER_WIDTH) as f32)
                    as u32
            });
        let status = match &state.status {
            RenderStatus::Ready { asset_key } => MermaidPreviewStatus::Ready {
                image_uri: image_uri(asset_key),
            },
            RenderStatus::Loading { last_good } => MermaidPreviewStatus::Loading {
                image_uri: last_good.as_deref().map(image_uri),
                message: if last_good.is_some() {
                    "Rendering updated Mermaid diagram…".to_owned()
                } else {
                    "Rendering Mermaid diagram…".to_owned()
                },
            },
            RenderStatus::Error { message, last_good } => MermaidPreviewStatus::Error {
                image_uri: last_good.as_deref().map(image_uri),
                message: if last_good.is_some() {
                    format!("Current Mermaid source has an error: {message}")
                } else {
                    format!("Mermaid rendering failed: {message}")
                },
            },
        };
        Some(MermaidPreview {
            index: state.block.index,
            diagram_label: state.block.diagram_label.clone(),
            experimental: state.block.support == SupportLevel::Experimental,
            display_width,
            status,
        })
    }
}

fn svg_intrinsic_width(svg: &[u8]) -> Option<f32> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).ok()?;
    let width = tree.size().width();
    width.is_finite().then_some(width.max(1.0))
}

pub fn discover_blocks(markdown_source: &str) -> Vec<MermaidBlock> {
    struct OpenBlock {
        whole_start: usize,
        body_start: Option<usize>,
        body_end: usize,
        source: String,
    }

    let mut blocks = Vec::new();
    let mut open: Option<OpenBlock> = None;
    for (event, event_range) in
        Parser::new_ext(markdown_source, markdown::parser_options()).into_offset_iter()
    {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info.trim().eq_ignore_ascii_case("mermaid") =>
            {
                open = Some(OpenBlock {
                    whole_start: event_range.start,
                    body_start: None,
                    body_end: event_range.end,
                    source: String::new(),
                });
            }
            Event::Text(text) if open.is_some() => {
                let block = open.as_mut().expect("checked above");
                block.body_start.get_or_insert(event_range.start);
                block.body_end = event_range.end;
                block.source.push_str(&text);
            }
            Event::End(TagEnd::CodeBlock) if open.is_some() => {
                let block = open.take().expect("checked above");
                let whole_range = block.whole_start..event_range.end;
                let body_start = block.body_start.unwrap_or(event_range.start);
                let body_range = body_start..block.body_end.max(body_start);
                let index = blocks.len();
                let (diagram_label, support) = classify_diagram(&block.source);
                blocks.push(MermaidBlock {
                    index,
                    source_key: content_key(&block.source),
                    anchor_key: anchor_key(markdown_source, &whole_range),
                    whole_range,
                    body_range,
                    source: block.source,
                    diagram_label,
                    support,
                });
            }
            _ => {}
        }
    }
    blocks
}

fn content_key(source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(RENDERER_CACHE_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(source.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn anchor_key(source: &str, range: &Range<usize>) -> String {
    let prefix_start = source[..range.start]
        .char_indices()
        .rev()
        .nth(64)
        .map_or(0, |(offset, _)| offset);
    let suffix_end = source[range.end..]
        .char_indices()
        .nth(64)
        .map_or(source.len(), |(offset, _)| range.end + offset);
    let mut hasher = Sha256::new();
    hasher.update(&source.as_bytes()[prefix_start..range.start]);
    hasher.update([0]);
    hasher.update(&source.as_bytes()[range.end..suffix_end]);
    format!("{:x}", hasher.finalize())
}

fn classify_diagram(source: &str) -> (String, SupportLevel) {
    let mut in_frontmatter = source.lines().next().map(str::trim) == Some("---");
    let token = source
        .lines()
        .skip(if in_frontmatter { 1 } else { 0 })
        .map(str::trim)
        .find(|line| {
            if in_frontmatter {
                if *line == "---" {
                    in_frontmatter = false;
                }
                return false;
            }
            !line.is_empty() && !line.starts_with("%%")
        })
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or("");
    let lower = token.to_ascii_lowercase();
    let (label, support) = match lower.as_str() {
        "erdiagram" => ("ER", SupportLevel::Supported),
        "flowchart" | "graph" => ("flowchart", SupportLevel::Supported),
        value if value.starts_with("statediagram") => ("state", SupportLevel::Supported),
        "classdiagram" => ("class", SupportLevel::Supported),
        "sequencediagram" => ("sequence", SupportLevel::Supported),
        "pie" => ("pie", SupportLevel::Supported),
        value if value.starts_with("packet") => ("packet", SupportLevel::Supported),
        "timeline" => ("timeline", SupportLevel::Supported),
        "journey" => ("journey", SupportLevel::Supported),
        "kanban" => ("kanban", SupportLevel::Supported),
        "gitgraph" => ("GitGraph", SupportLevel::Supported),
        "gantt" => ("Gantt", SupportLevel::Supported),
        value if value.starts_with("c4") => ("C4", SupportLevel::Supported),
        value if value.starts_with("block") => ("block", SupportLevel::Supported),
        value if value.starts_with("radar") => ("radar", SupportLevel::Supported),
        value if value.starts_with("treemap") => ("treemap", SupportLevel::Supported),
        value if value.starts_with("xychart") => ("XYChart", SupportLevel::Supported),
        "mindmap" => ("mindmap", SupportLevel::Supported),
        value if value.starts_with("architecture") => ("architecture", SupportLevel::Supported),
        "requirementdiagram" => ("requirement", SupportLevel::Supported),
        "quadrantchart" => ("quadrant", SupportLevel::Supported),
        value if value.starts_with("sankey") => ("Sankey", SupportLevel::Supported),
        value if value.starts_with("treeview") => ("TreeView", SupportLevel::Experimental),
        value if value.starts_with("ishikawa") => ("Ishikawa", SupportLevel::Experimental),
        value if value.starts_with("eventmodeling") => {
            ("Event Modeling", SupportLevel::Experimental)
        }
        value if value.starts_with("venn") => ("Venn", SupportLevel::Experimental),
        value
            if value.starts_with("zenuml")
                || value.starts_with("wardley")
                || value.starts_with("railroad")
                || value.starts_with("cynefin") =>
        {
            (token, SupportLevel::Unsupported)
        }
        _ => (
            if token.is_empty() { "unknown" } else { token },
            SupportLevel::Supported,
        ),
    };
    (label.to_owned(), support)
}

fn validate_source(source: &str, support: SupportLevel) -> Result<Cow<'_, str>, String> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "Mermaid source exceeds the {MAX_SOURCE_BYTES} byte limit"
        ));
    }
    if support == SupportLevel::Unsupported {
        return Err(
            "This diagram type is not supported by the Mermaid 11.15 compatibility baseline"
                .to_owned(),
        );
    }

    let lower = source.to_ascii_lowercase();
    let compact = lower
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    if compact.contains("%%{init") || compact.contains("%%{config") {
        return Err("Mermaid runtime configuration directives are disabled".to_owned());
    }
    for forbidden in ["http://", "https://", "file://", "data:"] {
        if lower.contains(forbidden) {
            return Err(format!(
                "Mermaid input contains a disabled directive or resource: {forbidden}"
            ));
        }
    }
    if source.lines().any(|line| {
        let line = line.trim_start().to_ascii_lowercase();
        line.starts_with("click ") || line.starts_with("href ")
    }) {
        return Err("Mermaid links and click actions are disabled".to_owned());
    }
    validate_frontmatter(source)?;
    normalize_line_break_tags(source)
}

fn normalize_line_break_tags(source: &str) -> Result<Cow<'_, str>, String> {
    let mut search_from = 0;
    let mut copied_through = 0;
    let mut normalized: Option<String> = None;

    while let Some(relative_start) = source[search_from..].find('<') {
        let start = search_from + relative_start;
        let tail = &source[start + 1..];
        let bytes = tail.as_bytes();
        let looks_like_html = match bytes.first().copied() {
            Some(first) if first.is_ascii_alphabetic() => true,
            Some(b'/') => bytes.get(1).is_some_and(|next| next.is_ascii_alphabetic()),
            Some(b'!') => true,
            _ => false,
        };
        if !looks_like_html {
            search_from = start + 1;
            continue;
        }

        let Some(relative_end) = tail.find('>') else {
            return Err("HTML labels are disabled in Mermaid diagrams".to_owned());
        };
        let end = start + 1 + relative_end;
        let tag_body = &source[start + 1..end];
        let is_plain_break = tag_body.get(..2).is_some_and(|name| {
            name.eq_ignore_ascii_case("br") && {
                let remainder = tag_body[2..].trim();
                remainder.is_empty() || remainder == "/"
            }
        });
        if !is_plain_break {
            return Err(
                "HTML labels are disabled; only plain <br> line breaks are allowed".to_owned(),
            );
        }

        let output = normalized.get_or_insert_with(|| String::with_capacity(source.len()));
        output.push_str(&source[copied_through..start]);
        output.push_str("<br/>");
        copied_through = end + 1;
        search_from = end + 1;
    }

    if let Some(mut normalized) = normalized {
        normalized.push_str(&source[copied_through..]);
        Ok(Cow::Owned(normalized))
    } else {
        Ok(Cow::Borrowed(source))
    }
}

fn validate_frontmatter(source: &str) -> Result<(), String> {
    let mut lines = source.lines();
    if lines.next().map(str::trim) != Some("---") {
        return Ok(());
    }
    let mut closed = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            closed = true;
            break;
        }
        if trimmed.is_empty() || trimmed.starts_with("title:") {
            continue;
        }
        return Err("Only the frontmatter title field is allowed in Mermaid blocks".to_owned());
    }
    if !closed {
        return Err("Mermaid frontmatter is not closed".to_owned());
    }
    Ok(())
}

fn validate_svg(svg: &str) -> Result<(), String> {
    if svg.len() > MAX_SVG_BYTES {
        return Err(format!(
            "Rendered SVG exceeds the {MAX_SVG_BYTES} byte limit"
        ));
    }
    let lower = svg.to_ascii_lowercase();
    for forbidden in [
        "<script",
        "<foreignobject",
        "<a ",
        "javascript:",
        "data:",
        "file://",
        "xlink:href",
        "href=\"http",
        "href='http",
        "href=\"//",
        "href='//",
        "url(http",
        "onload=",
        "onclick=",
    ] {
        if lower.contains(forbidden) {
            return Err(format!(
                "Rendered SVG contains forbidden content: {forbidden}"
            ));
        }
    }
    let elements = svg
        .as_bytes()
        .windows(2)
        .filter(|pair| pair[0] == b'<' && pair[1].is_ascii_alphabetic())
        .count();
    if elements > MAX_SVG_ELEMENTS {
        return Err(format!(
            "Rendered SVG exceeds the {MAX_SVG_ELEMENTS} element limit"
        ));
    }
    Ok(())
}

fn clean_error(error: &str) -> String {
    let mut cleaned = error
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(4)
        .collect::<Vec<_>>()
        .join(" · ")
        .replace("```", "'''");
    if cleaned.is_empty() {
        cleaned = "Mermaid rendering failed".to_owned();
    }
    if cleaned.len() > 1_000 {
        cleaned.truncate(1_000);
        cleaned.push('…');
    }
    cleaned
}

#[derive(Clone)]
pub struct MermaidWorker {
    requests: mpsc::Sender<WorkerRequest>,
    next_id: Arc<AtomicU64>,
}

struct WorkerRequest {
    id: u64,
    source: String,
    timeout: Duration,
    reply: oneshot::Sender<Result<String, String>>,
}

impl MermaidWorker {
    fn new() -> Self {
        let (requests, receiver) = mpsc::channel::<WorkerRequest>();
        thread::Builder::new()
            .name("mermaid-worker-manager".to_owned())
            .spawn(move || worker_manager(receiver))
            .expect("failed to start Mermaid worker manager");
        Self {
            requests,
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn render(
        &self,
        source: String,
        timeout: Duration,
    ) -> oneshot::Receiver<Result<String, String>> {
        let (reply, result) = oneshot::channel();
        let request = WorkerRequest {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            source,
            timeout,
            reply,
        };
        if let Err(error) = self.requests.send(request) {
            let _ = error
                .0
                .reply
                .send(Err("Mermaid worker is unavailable".to_owned()));
        }
        result
    }
}

struct WorkerResponse {
    id: u64,
    result: Result<String, String>,
}

struct WorkerProcess {
    child: Child,
    stdin: ChildStdin,
    responses: mpsc::Receiver<Result<WorkerResponse, String>>,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

impl WorkerProcess {
    fn spawn() -> Result<Self, String> {
        let executable = std::env::current_exe()
            .map_err(|error| format!("could not locate Mermaid worker executable: {error}"))?;
        let mut command = Command::new(executable);
        command
            .arg(WORKER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            command.creation_flags(0x0800_0000);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start Mermaid worker: {error}"))?;
        #[cfg(windows)]
        let job = attach_worker_job(&mut child)?;
        WORKER_PID.store(u64::from(child.id()), Ordering::Relaxed);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Mermaid worker stdin was unavailable".to_owned())?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Mermaid worker stdout was unavailable".to_owned())?;
        let (responses, receiver) = mpsc::channel();
        thread::Builder::new()
            .name("mermaid-worker-reader".to_owned())
            .spawn(move || loop {
                let response = read_worker_response(&mut stdout);
                let finished = response.is_err();
                if responses.send(response).is_err() || finished {
                    break;
                }
            })
            .map_err(|error| format!("could not start Mermaid response reader: {error}"))?;
        Ok(Self {
            child,
            stdin,
            responses: receiver,
            #[cfg(windows)]
            job,
        })
    }

    fn request(&mut self, request: &WorkerRequest) -> Result<Result<String, String>, String> {
        write_request(&mut self.stdin, request.id, request.source.as_bytes())?;
        match self.responses.recv_timeout(request.timeout) {
            Ok(Ok(response)) if response.id == request.id => Ok(response.result),
            Ok(Ok(response)) => Err(format!(
                "Mermaid worker protocol mismatch: expected {}, received {}",
                request.id, response.id
            )),
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let _ = self.child.kill();
                let _ = self.child.wait();
                Err(format!(
                    "Mermaid rendering exceeded the {} ms time limit",
                    request.timeout.as_millis()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("Mermaid worker stopped unexpectedly".to_owned())
            }
        }
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        WORKER_PID
            .compare_exchange(
                u64::from(self.child.id()),
                0,
                Ordering::Relaxed,
                Ordering::Relaxed,
            )
            .ok();
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
    }
}

#[cfg(windows)]
fn attach_worker_job(child: &mut Child) -> Result<windows_sys::Win32::Foundation::HANDLE, String> {
    use std::mem::{size_of, zeroed};
    use std::os::windows::io::AsRawHandle as _;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    };

    let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if job.is_null() {
        return Err(format!(
            "could not create Mermaid worker job: {}",
            io::Error::last_os_error()
        ));
    }
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { zeroed() };
    limits.BasicLimitInformation.LimitFlags =
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
    limits.ProcessMemoryLimit = 96 * 1024 * 1024;
    let configured = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    let assigned = if configured != 0 {
        unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) }
    } else {
        0
    };
    if configured == 0 || assigned == 0 {
        unsafe {
            CloseHandle(job);
        }
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!(
            "could not constrain Mermaid worker: {}",
            io::Error::last_os_error()
        ));
    }
    Ok(job)
}

fn worker_manager(requests: mpsc::Receiver<WorkerRequest>) {
    let mut process: Option<WorkerProcess> = None;
    for request in requests {
        if process.is_none() {
            process = WorkerProcess::spawn().ok();
        }
        let outcome = match process.as_mut() {
            Some(worker) => worker.request(&request),
            None => Err("Mermaid worker could not be started".to_owned()),
        };
        let result = match outcome {
            Ok(render_result) => render_result,
            Err(transport_error) => {
                process = None;
                Err(transport_error)
            }
        };
        let _ = request.reply.send(result);
    }
}

fn write_request(writer: &mut impl Write, id: u64, payload: &[u8]) -> Result<(), String> {
    if payload.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "Mermaid source exceeds the {MAX_SOURCE_BYTES} byte limit"
        ));
    }
    writer
        .write_all(&id.to_le_bytes())
        .and_then(|_| writer.write_all(&(payload.len() as u32).to_le_bytes()))
        .and_then(|_| writer.write_all(payload))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("could not send Mermaid render request: {error}"))
}

fn read_worker_response(reader: &mut impl Read) -> Result<WorkerResponse, String> {
    let id =
        read_u64(reader).map_err(|error| format!("could not read Mermaid response: {error}"))?;
    let mut status = [0_u8; 1];
    reader
        .read_exact(&mut status)
        .map_err(|error| format!("could not read Mermaid response status: {error}"))?;
    let length = read_u32(reader)
        .map_err(|error| format!("could not read Mermaid response length: {error}"))?
        as usize;
    if length > MAX_SVG_BYTES.max(MAX_ERROR_BYTES) {
        return Err("Mermaid worker response exceeded the protocol limit".to_owned());
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .map_err(|error| format!("could not read Mermaid response body: {error}"))?;
    let payload = String::from_utf8(payload)
        .map_err(|_| "Mermaid worker returned invalid UTF-8".to_owned())?;
    Ok(WorkerResponse {
        id,
        result: if status[0] == 0 {
            Ok(payload)
        } else {
            Err(payload)
        },
    })
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

pub fn run_worker_if_requested() -> bool {
    match std::env::args().nth(1).as_deref() {
        Some(WORKER_ARG) => {
            if let Err(error) = run_worker() {
                eprintln!("Mermaid worker failed: {error}");
                std::process::exit(2);
            }
            true
        }
        Some(SELF_TEST_ARG) => {
            let worker = MermaidWorker::new();
            let result = smol::block_on(
                worker.render("flowchart LR\nA[开始]-->B[完成]".to_owned(), OPEN_TIMEOUT),
            )
            .unwrap_or_else(|_| Err("Mermaid self-test channel closed".to_owned()));
            match result {
                Ok(svg) if svg.starts_with("<svg") => {
                    println!("MERMAID_SELF_TEST result=pass bytes={}", svg.len());
                    true
                }
                Ok(_) => {
                    eprintln!("MERMAID_SELF_TEST result=fail error=invalid_svg");
                    std::process::exit(2);
                }
                Err(error) => {
                    eprintln!("MERMAID_SELF_TEST result=fail error={error:?}");
                    std::process::exit(2);
                }
            }
        }
        _ => false,
    }
}

fn run_worker() -> Result<(), String> {
    let profile = HostThemeProfile::builder()
        .font_family("Segoe UI, Microsoft YaHei, sans-serif")
        .roles(HostThemeRoles {
            canvas: Some("#faf7f0".to_owned()),
            surface: Some("#f6f1e7".to_owned()),
            surface_alt: Some("#ebe3d4".to_owned()),
            text: Some("#292723".to_owned()),
            border: Some("#a99d89".to_owned()),
            line: Some("#70685b".to_owned()),
            note_background: Some("#fff4cf".to_owned()),
            note_border: Some("#b6923e".to_owned()),
            note_text: Some("#292723".to_owned()),
            ..HostThemeRoles::default()
        })
        .series_palette(["#587c8d", "#b66a4c", "#718052", "#9575a6", "#c1933d"])
        .output(HostThemeOutput::resvg_safe_editor())
        .build();
    let renderer = HeadlessRenderer::new()
        .with_strict_parsing()
        .with_host_theme(&profile)
        .with_vendored_text_measurer()
        .with_diagram_id("native-markdown-mermaid");
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = stdin.lock();
    let mut writer = stdout.lock();

    loop {
        let id = match read_u64(&mut reader) {
            Ok(id) => id,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(format!("could not read request id: {error}")),
        };
        let length = read_u32(&mut reader)
            .map_err(|error| format!("could not read request length: {error}"))?
            as usize;
        if length > MAX_SOURCE_BYTES {
            write_response(
                &mut writer,
                id,
                &Err(format!(
                    "Mermaid source exceeds the {MAX_SOURCE_BYTES} byte limit"
                )),
            )?;
            return Err("worker received an oversized request".to_owned());
        }
        let mut source = vec![0_u8; length];
        reader
            .read_exact(&mut source)
            .map_err(|error| format!("could not read request body: {error}"))?;
        let result = String::from_utf8(source)
            .map_err(|_| "Mermaid source is not valid UTF-8".to_owned())
            .and_then(|source| render_one(&renderer, &source));
        write_response(&mut writer, id, &result)?;
    }
}

fn render_one(renderer: &HeadlessRenderer, source: &str) -> Result<String, String> {
    let (_, support) = classify_diagram(source);
    let source = validate_source(source, support)?;
    let svg = renderer
        .render_svg_sync(&source)
        .map_err(|error| clean_error(&error.to_string()))?
        .ok_or_else(|| "No Mermaid diagram was detected".to_owned())?;
    validate_svg(&svg)?;
    Ok(svg)
}

fn write_response(
    writer: &mut impl Write,
    id: u64,
    result: &Result<String, String>,
) -> Result<(), String> {
    let (status, payload) = match result {
        Ok(svg) => (0_u8, svg.as_bytes()),
        Err(error) => (1_u8, error.as_bytes()),
    };
    let payload = &payload[..payload.len().min(if status == 0 {
        MAX_SVG_BYTES
    } else {
        MAX_ERROR_BYTES
    })];
    writer
        .write_all(&id.to_le_bytes())
        .and_then(|_| writer.write_all(&[status]))
        .and_then(|_| writer.write_all(&(payload.len() as u32).to_le_bytes()))
        .and_then(|_| writer.write_all(payload))
        .and_then(|_| writer.flush())
        .map_err(|error| format!("could not write Mermaid response: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_case_insensitive_mermaid_fences_with_original_ranges() {
        let source = "before\n\n``` Mermaid \nflowchart LR\nA-->B\n```\n\nafter";
        let blocks = discover_blocks(source);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].source, "flowchart LR\nA-->B\n");
        assert_eq!(&source[blocks[0].body_range.clone()], blocks[0].source);
        assert_eq!(
            &source[blocks[0].whole_range.clone()],
            "``` Mermaid \nflowchart LR\nA-->B\n```"
        );
    }

    #[test]
    fn ignores_non_mermaid_code_blocks() {
        let source = "```rust\nfn main() {}\n```";
        assert!(discover_blocks(source).is_empty());
    }

    #[test]
    fn preview_lookup_uses_the_original_fence_range() {
        let source = "# Title\n\n```mermaid\nflowchart LR\nA-->B\n```\n\nTail\n";
        let mut manager = MermaidManager::new();
        let jobs = manager.refresh(source, EDIT_TIMEOUT);
        assert_eq!(jobs.len(), 1);
        let range = discover_blocks(source)[0].whole_range.clone();
        let preview = manager.preview_for_block(range.clone(), 900, 100).unwrap();
        assert!(matches!(
            preview.status,
            MermaidPreviewStatus::Loading {
                image_uri: None,
                ref message,
            } if message == "Rendering Mermaid diagram…"
        ));
        assert!(manager
            .preview_for_block(range.start + 1..range.end, 900, 100)
            .is_none());
    }

    #[test]
    fn rejects_runtime_config_and_external_resources() {
        assert!(validate_source(
            "%%{init: {'theme': 'dark'}}%%\nflowchart LR\nA-->B",
            SupportLevel::Supported
        )
        .is_err());
        assert!(validate_source(
            "flowchart LR\nA[https://example.com]",
            SupportLevel::Supported
        )
        .is_err());
        assert!(validate_source(
            "flowchart LR\nA[<b>unsafe label</b>]",
            SupportLevel::Supported
        )
        .is_err());
        assert!(validate_source("classDiagram\nBase <|-- Child", SupportLevel::Supported).is_ok());
    }

    #[test]
    fn normalizes_plain_break_tags_without_allowing_html_attributes() {
        let source = "flowchart LR\nA[one<br>two] --> B[three<BR/>four] --> C[five<br />six]";
        let normalized = validate_source(source, SupportLevel::Supported).unwrap();
        assert_eq!(
            normalized,
            "flowchart LR\nA[one<br/>two] --> B[three<br/>four] --> C[five<br/>six]"
        );

        for source in [
            "flowchart LR\nA[<b>bold</b>]",
            "flowchart LR\nA[one<br class='wide'>two]",
            "flowchart LR\nA[one<br onclick='run()'>two]",
            "flowchart LR\nA[one</br>two]",
        ] {
            assert!(
                validate_source(source, SupportLevel::Supported).is_err(),
                "{source}"
            );
        }
    }

    #[test]
    fn allows_title_only_frontmatter() {
        assert!(validate_source(
            "---\ntitle: Example\n---\nflowchart LR\nA-->B",
            SupportLevel::Supported
        )
        .is_ok());
        assert!(validate_source(
            "---\ntitle: Example\nconfig:\n  theme: dark\n---\nflowchart LR\nA-->B",
            SupportLevel::Supported
        )
        .is_err());
    }

    #[test]
    fn protocol_round_trip_preserves_success_and_error() {
        let mut bytes = Vec::new();
        write_response(&mut bytes, 7, &Ok("<svg/>".to_owned())).unwrap();
        let response = read_worker_response(&mut bytes.as_slice()).unwrap();
        assert_eq!(response.id, 7);
        assert_eq!(response.result.unwrap(), "<svg/>");

        bytes.clear();
        write_response(&mut bytes, 8, &Err("bad diagram".to_owned())).unwrap();
        let response = read_worker_response(&mut bytes.as_slice()).unwrap();
        assert_eq!(response.id, 8);
        assert_eq!(response.result.unwrap_err(), "bad diagram");
    }

    #[test]
    fn stale_render_result_cannot_replace_the_current_block() {
        let first = "before\n```mermaid\nflowchart LR\nA-->B\n```\nafter";
        let second = "before\n```mermaid\nflowchart LR\nA-->C\n```\nafter";
        let mut manager = MermaidManager::new();
        let first_job = manager.refresh(first, EDIT_TIMEOUT).remove(0);
        let fake_svg: Arc<[u8]> = Arc::from(b"<svg/>".as_slice());
        assert!(manager
            .apply_result(&first_job.source_key, Ok("<svg/>".to_owned()))
            .is_some());

        let second_job = manager.refresh(second, EDIT_TIMEOUT).remove(0);
        let range = discover_blocks(second)[0].whole_range.clone();
        let preview = manager.preview_for_block(range, 900, 100).unwrap();
        assert!(matches!(
            preview.status,
            MermaidPreviewStatus::Loading {
                image_uri: Some(ref uri),
                ref message,
            } if uri.contains(&first_job.source_key)
                && message == "Rendering updated Mermaid diagram…"
        ));
        assert!(manager
            .apply_result(
                &first_job.source_key,
                Ok(String::from_utf8(fake_svg.to_vec()).unwrap())
            )
            .is_none());
        assert!(manager.needs_result(&second_job.source_key));
    }

    #[test]
    fn document_zoom_reuses_the_same_mermaid_image_resource() {
        let source = "```mermaid\nflowchart LR\nA-->B\n```";
        let mut manager = MermaidManager::new();
        let job = manager.refresh(source, EDIT_TIMEOUT).remove(0);
        assert!(manager
            .apply_result(
                &job.source_key,
                Ok(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" width="300" height="120"></svg>"#
                        .to_owned()
                ),
            )
            .is_some());
        let range = discover_blocks(source)[0].whole_range.clone();

        let preview_at_100 = manager.preview_for_block(range.clone(), 900, 100).unwrap();
        assert_eq!(preview_at_100.display_width, Some(300));
        let uri_at_100 = match preview_at_100.status {
            MermaidPreviewStatus::Ready { image_uri } => image_uri,
            status => panic!("expected ready preview, got {status:?}"),
        };
        let preview_at_200 = manager.preview_for_block(range.clone(), 900, 200).unwrap();
        assert_eq!(preview_at_200.display_width, Some(600));
        let uri_at_200 = match preview_at_200.status {
            MermaidPreviewStatus::Ready { image_uri } => image_uri,
            status => panic!("expected ready preview, got {status:?}"),
        };

        assert_eq!(uri_at_100, uri_at_200);
        assert_eq!(
            manager
                .preview_for_block(range, 500, 250)
                .unwrap()
                .display_width,
            Some(500)
        );
    }

    #[test]
    fn compatibility_matrix_classifies_supported_and_experimental_families() {
        for source in [
            "erDiagram",
            "flowchart LR",
            "stateDiagram-v2",
            "classDiagram",
            "sequenceDiagram",
            "pie",
            "packet-beta",
            "timeline",
            "journey",
            "kanban",
            "gitGraph",
            "gantt",
            "C4Context",
            "block-beta",
            "radar-beta",
            "treemap-beta",
            "xychart-beta",
            "mindmap",
            "architecture-beta",
            "requirementDiagram",
            "quadrantChart",
            "sankey-beta",
        ] {
            assert_eq!(
                classify_diagram(source).1,
                SupportLevel::Supported,
                "{source}"
            );
        }
        for source in [
            "treeView-beta",
            "ishikawa-beta",
            "eventmodeling",
            "venn-beta",
        ] {
            assert_eq!(
                classify_diagram(source).1,
                SupportLevel::Experimental,
                "{source}"
            );
        }
        for source in ["zenuml", "wardley", "railroad", "cynefin"] {
            assert_eq!(
                classify_diagram(source).1,
                SupportLevel::Unsupported,
                "{source}"
            );
        }
    }

    #[test]
    fn frontmatter_title_does_not_hide_the_diagram_type() {
        let (label, support) = classify_diagram("---\ntitle: 中文流程\n---\nflowchart LR\nA-->B");
        assert_eq!(label, "flowchart");
        assert_eq!(support, SupportLevel::Supported);
    }

    #[test]
    fn document_diagram_limit_turns_excess_blocks_into_errors() {
        let source = (0..=MAX_DOCUMENT_DIAGRAMS)
            .map(|index| format!("```mermaid\nflowchart LR\nA{index}-->B{index}\n```"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut manager = MermaidManager::new();
        let jobs = manager.refresh(&source, EDIT_TIMEOUT);
        assert_eq!(jobs.len(), MAX_DOCUMENT_DIAGRAMS);
        assert_eq!(manager.error_count(), 1);
    }
}
