//! Safe, offline HTML projection. It never mutates the Markdown source or exposes a filesystem URL.
use pulldown_cmark::{html, Event, Options, Parser, Tag};
use std::{
    borrow::Cow,
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

include!("../assets/web/embedded.rs");

pub const ORIGIN: &str = "http://reader.localhost";
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self'; font-src 'self'; connect-src 'self'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'";
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Default)]
pub struct Resources {
    generation: u64,
    images: HashMap<String, (PathBuf, PathBuf)>,
    links: HashMap<String, PathBuf>,
    html: String,
}

impl Resources {
    pub fn render(&mut self, source: &str, document: Option<&Path>) -> String {
        self.generation = self.generation.wrapping_add(1);
        self.images.clear();
        self.links.clear();
        let root = document
            .and_then(Path::parent)
            .and_then(|p| p.canonicalize().ok());
        let mut options = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_GFM;
        options.insert(Options::ENABLE_MATH);
        let mut depth = 0usize;
        let events = Parser::new_ext(source, options)
            .map(|event| match event {
                Event::Start(Tag::Link {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) if !dest_url.starts_with('#') => {
                    let destination = root
                        .as_ref()
                        .and_then(|root| local_document(root, &dest_url))
                        .map(|path| {
                            let uri = format!("/link/{}/{}", self.generation, self.links.len());
                            self.links.insert(uri.clone(), path);
                            uri
                        })
                        .unwrap_or_else(|| dest_url.to_string());
                    Event::Start(Tag::Link {
                        link_type,
                        dest_url: destination.into(),
                        title,
                        id,
                    })
                }
                Event::Start(Tag::Image {
                    link_type,
                    dest_url,
                    title,
                    id,
                }) => {
                    let uri = root
                        .as_ref()
                        .and_then(|root| local_image(root, &dest_url))
                        .map(|path| {
                            let uri = format!("/image/{}/{}", self.generation, self.images.len());
                            self.images
                                .insert(uri.clone(), (root.as_ref().unwrap().clone(), path));
                            uri
                        })
                        .unwrap_or_default();
                    Event::Start(Tag::Image {
                        link_type,
                        dest_url: uri.into(),
                        title,
                        id,
                    })
                }
                Event::InlineMath(text) => Event::Html(
                    format!("<span class=\"math-inline\">{}</span>", escape(&text)).into(),
                ),
                Event::DisplayMath(text) => Event::Html(
                    format!("<div class=\"math-display\">{}</div>", escape(&text)).into(),
                ),
                Event::TaskListMarker(checked) => Event::Html(
                    if checked {
                        "<span class=\"task-marker\">☑</span> "
                    } else {
                        "<span class=\"task-marker\">☐</span> "
                    }
                    .into(),
                ),
                // Safe HTML tables and inline formatting pass through the sanitizer below. Raw HTML
                // images do not acquire filesystem access: only generated /image tokens are served.
                event => event,
            })
            .flat_map(|event| {
                let boundary = depth == 0 && matches!(event, Event::Start(Tag::Heading { .. }));
                match &event {
                    Event::Start(_) => depth += 1,
                    Event::End(_) => depth = depth.saturating_sub(1),
                    _ => {}
                }
                let mut events = Vec::with_capacity(2);
                if boundary {
                    events.push(Event::Html(
                        "</section><section class=\"reading-section\">".into(),
                    ));
                }
                events.push(event);
                events
            });
        let mut output = String::from("<section class=\"reading-section\">");
        html::push_html(&mut output, events);
        output.push_str("</section>");
        self.html = ammonia::Builder::default()
            .add_tags(&["section"])
            .add_tag_attributes("td", &["colspan", "rowspan"])
            .add_tag_attributes("th", &["colspan", "rowspan"])
            .add_generic_attributes(&["class"])
            .url_schemes(["http", "https", "mailto"].into_iter().collect())
            .clean(&output)
            .to_string();
        self.html.clone()
    }

    pub fn image_count(&self) -> usize {
        self.images.len()
    }

    pub fn link_target(&self, id: &str) -> Option<PathBuf> {
        self.links.get(id).cloned()
    }
}

fn local_document(root: &Path, reference: &str) -> Option<PathBuf> {
    let reference = reference.split(['?', '#']).next()?;
    let decoded = percent_encoding::percent_decode_str(reference)
        .decode_utf8()
        .ok()?;
    if decoded.contains(':') || decoded.starts_with(['/', '\\']) {
        return None;
    }
    let relative = PathBuf::from(decoded.replace('/', std::path::MAIN_SEPARATOR_STR));
    if relative.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }
    let path = root.join(relative).canonicalize().ok()?;
    if !path.starts_with(root) || !path.is_file() {
        return None;
    }
    matches!(
        path.extension()?.to_str()?.to_ascii_lowercase().as_str(),
        "md" | "markdown"
    )
    .then_some(path)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn local_image(root: &Path, reference: &str) -> Option<PathBuf> {
    let reference = reference.split(['?', '#']).next()?;
    let decoded = percent_encoding::percent_decode_str(reference)
        .decode_utf8()
        .ok()?;
    if decoded.contains(':') || decoded.starts_with(['/', '\\']) {
        return None;
    }
    let relative = PathBuf::from(decoded.replace('/', std::path::MAIN_SEPARATOR_STR));
    if relative.components().any(|c| {
        matches!(
            c,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return None;
    }
    let path = root.join(relative).canonicalize().ok()?;
    if !path.starts_with(root) || image_type(&path).is_none() {
        return None;
    }
    let meta = path.metadata().ok()?;
    (meta.is_file() && meta.len() <= MAX_IMAGE_BYTES).then_some(path)
}

fn image_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "svg" => Some("image/svg+xml"),
        "bmp" => Some("image/bmp"),
        _ => None,
    }
}

pub fn respond(
    resources: &Arc<RwLock<Resources>>,
    path: &str,
) -> wry::http::Response<Cow<'static, [u8]>> {
    let (mime, body): (&str, Option<Cow<'static, [u8]>>) = match path {
        "/" => (
            "text/html; charset=utf-8",
            Some(Cow::Owned(
                include_str!("../assets/web/index.html")
                    .replace("<!--DOCUMENT-->", &resources.read().unwrap().html)
                    .into_bytes(),
            )),
        ),
        "/document" => (
            "text/plain; charset=utf-8",
            Some(Cow::Owned(
                resources.read().unwrap().html.as_bytes().to_vec(),
            )),
        ),
        "/reader.js" => (
            "text/javascript",
            Some(Cow::Borrowed(include_bytes!("../assets/web/reader.js"))),
        ),
        "/reader.css" => (
            "text/css",
            Some(Cow::Borrowed(include_bytes!("../assets/web/reader.css"))),
        ),
        path if path.starts_with("/vendor/") => {
            let mime = if path.ends_with(".js") {
                "text/javascript"
            } else if path.ends_with(".css") {
                "text/css"
            } else {
                "font/woff2"
            };
            (mime, embedded_asset(&path[1..]).map(Cow::Borrowed))
        }
        _ => {
            let state = resources.read().unwrap();
            let image = state.images.get(path).and_then(|(root, p)| {
                // Revalidate canonical confinement on every read (including changed symlinks).
                let canonical = p.canonicalize().ok()?;
                if !canonical.starts_with(root) {
                    return None;
                }
                let metadata = canonical.metadata().ok()?;
                if metadata.len() > MAX_IMAGE_BYTES {
                    return None;
                }
                let mut data = Vec::new();
                std::fs::File::open(&canonical)
                    .ok()?
                    .take(MAX_IMAGE_BYTES + 1)
                    .read_to_end(&mut data)
                    .ok()?;
                if data.len() as u64 > MAX_IMAGE_BYTES {
                    return None;
                }
                Some((image_type(p)?, data))
            });
            match image {
                Some((mime, data)) => (mime, Some(Cow::Owned(data))),
                None => ("text/plain", None),
            }
        }
    };
    wry::http::Response::builder()
        .status(if body.is_some() { 200 } else { 404 })
        .header("Content-Type", mime)
        .header("Content-Security-Policy", CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header("Cache-Control", "no-store")
        .body(body.unwrap_or(Cow::Borrowed(b"Not found")))
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn untrusted_source_cannot_create_executable_markup_or_ipc() {
        let mut resources = Resources::default();
        let html = resources.render("# Safe\n\n<script>window.ipc.postMessage('save')</script>\n\n<img src=file:///secret onerror=alert(1)>\n\n[bad](javascript:alert(1))\n\n![bad](../secret.png)\n\n$E=mc^2$", None);
        assert!(!html.contains("<script"));
        assert!(!html.contains("onerror"));
        assert!(!html.contains("javascript:"));
        assert!(!html.contains("file:"));
        assert!(html.contains("math-inline"));
        assert!(html.contains("<h1>Safe</h1>"));
        assert!(resources.images.is_empty());
    }
    #[test]
    fn stale_image_and_arbitrary_paths_are_not_served() {
        let resources = Arc::new(RwLock::new(Resources::default()));
        assert_eq!(respond(&resources, "/image/0/0").status(), 404);
        assert_eq!(respond(&resources, "/../../Cargo.toml").status(), 404);
        assert_eq!(
            respond(&resources, "/vendor/../../Cargo.toml").status(),
            404
        );
        assert_eq!(respond(&resources, "/").status(), 200);
    }

    #[test]
    fn only_document_images_and_markdown_links_receive_generation_tokens() {
        let folder = tempfile::tempdir().unwrap();
        let document = folder.path().join("document.md");
        std::fs::write(&document, "# Document").unwrap();
        std::fs::write(folder.path().join("picture space.svg"), "<svg/>").unwrap();
        std::fs::write(folder.path().join("next.md"), "# Next").unwrap();
        std::fs::write(folder.path().join("secret.txt"), "secret").unwrap();
        let resources = Arc::new(RwLock::new(Resources::default()));
        let html = resources.write().unwrap().render("![local](picture%20space.svg) [next](next.md) ![escape](%2e%2e/secret.svg) ![secret](secret.txt)", Some(&document));
        assert!(html.contains("/image/1/0"));
        assert!(html.contains("/link/1/0"));
        assert_eq!(resources.read().unwrap().image_count(), 1);
        assert_eq!(respond(&resources, "/image/1/0").status(), 200);
        assert!(resources.read().unwrap().link_target("/link/1/0").is_some());
        resources
            .write()
            .unwrap()
            .render("# Different document", Some(&document));
        assert_eq!(respond(&resources, "/image/1/0").status(), 404);
        assert!(resources.read().unwrap().link_target("/link/1/0").is_none());
    }

    #[test]
    fn safe_html_table_spans_survive_but_active_content_does_not() {
        let html = Resources::default().render("<table><tr><td colspan=\"2\" onclick=\"alert(1)\">Merged</td></tr></table>\n<iframe src=\"file:///secret\"></iframe>", None);
        assert!(html.contains("colspan=\"2\""));
        assert!(html.contains("Merged"));
        assert!(!html.contains("onclick"));
        assert!(!html.contains("iframe"));
    }

    #[test]
    fn task_states_survive_without_allowing_document_form_controls() {
        let html = Resources::default().render("- [x] Complete\n- [ ] Pending", None);
        assert!(html.contains('☑'));
        assert!(html.contains('☐'));
        assert!(!html.contains("<input"));
    }
}
