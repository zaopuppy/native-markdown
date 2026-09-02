use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use anyhow::{bail, Context as _};
use futures::{future::BoxFuture, AsyncReadExt as _};
use gpui_http_client::http::HeaderValue;
use gpui_http_client::{AsyncBody, HttpClient, Request, Response, StatusCode, Url};
use image::{imageops::FilterType, ImageFormat};

use crate::mermaid::{MAX_RASTER_PIXELS, MAX_RASTER_WIDTH, URI_PREFIX};

// Markdown images never need more decoded pixels than the reading surface can display. A bounded
// physical width also keeps ultra-wide source figures from dominating the process heap.
const MAX_PREPARED_IMAGE_WIDTH: u32 = MAX_RASTER_WIDTH;
const MAX_MERMAID_SVG_STORE_BYTES: usize = 16 * 1024 * 1024;
pub const VIEWER_MAX_IMAGE_WIDTH: u32 = 8_192;
pub const VIEWER_MAX_IMAGE_PIXELS: u64 = 8 * 1024 * 1024;
pub const VIEWER_URI_PREFIX: &str = "native-markdown-viewer://";
type PreparedImageCache = Arc<Mutex<HashMap<(PathBuf, u32, u64), Arc<[u8]>>>>;

#[derive(Clone, Copy)]
struct ImagePreparation {
    max_width: u32,
    max_pixels: u64,
    retain: bool,
}

#[derive(Clone)]
struct ViewerImageRequest {
    source: String,
    generation: u64,
}

#[derive(Default)]
struct MermaidSvgStore {
    entries: HashMap<String, Arc<[u8]>>,
    bytes: usize,
}

#[derive(Clone)]
pub struct DocumentImageRoot {
    root: Arc<RwLock<Option<PathBuf>>>,
    loads: Arc<AtomicUsize>,
    requested_resources: Arc<RwLock<HashSet<String>>>,
    viewport_width: Arc<AtomicU32>,
    generation: Arc<AtomicU64>,
    document_gate: Arc<Mutex<()>>,
    preparation_gate: Arc<Mutex<()>>,
    prepared_images: PreparedImageCache,
    mermaid_svgs: Arc<RwLock<MermaidSvgStore>>,
    viewer_requests: Arc<RwLock<HashMap<String, ViewerImageRequest>>>,
    next_viewer_request: Arc<AtomicU64>,
}

impl Default for DocumentImageRoot {
    fn default() -> Self {
        Self {
            root: Arc::default(),
            loads: Arc::default(),
            requested_resources: Arc::default(),
            viewport_width: Arc::new(AtomicU32::new(MAX_PREPARED_IMAGE_WIDTH)),
            generation: Arc::default(),
            document_gate: Arc::default(),
            preparation_gate: Arc::default(),
            prepared_images: Arc::default(),
            mermaid_svgs: Arc::default(),
            viewer_requests: Arc::default(),
            next_viewer_request: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl DocumentImageRoot {
    pub fn set_document_path(&self, path: Option<&Path>) {
        let _document_guard = self
            .document_gate
            .lock()
            .expect("document image state lock poisoned");
        let root = path
            .and_then(Path::parent)
            .and_then(|path| path.canonicalize().ok());
        *self.root.write().expect("image root lock poisoned") = root;
        self.generation.fetch_add(1, Ordering::Relaxed);
        self.loads.store(0, Ordering::Relaxed);
        self.prepared_images
            .lock()
            .expect("prepared image cache lock poisoned")
            .clear();
        self.viewer_requests
            .write()
            .expect("viewer image request lock poisoned")
            .clear();
        let mut mermaid = self
            .mermaid_svgs
            .write()
            .expect("Mermaid SVG store lock poisoned");
        mermaid.entries.clear();
        mermaid.bytes = 0;
    }

    pub fn load_count(&self) -> usize {
        self.loads.load(Ordering::Relaxed)
    }

    pub fn take_requested_resources(&self) -> Vec<String> {
        self.requested_resources
            .write()
            .expect("requested image resource lock poisoned")
            .drain()
            .collect()
    }

    pub fn set_viewport_width(&self, width: u32) {
        self.viewport_width
            .store(width.clamp(1, MAX_PREPARED_IMAGE_WIDTH), Ordering::Relaxed);
    }

    pub fn viewer_uri(&self, source: &str) -> String {
        let generation = self.generation.load(Ordering::Relaxed);
        let request = self.next_viewer_request.fetch_add(1, Ordering::Relaxed);
        let uri = format!("{VIEWER_URI_PREFIX}{generation}/{request}");
        self.viewer_requests
            .write()
            .expect("viewer image request lock poisoned")
            .insert(
                uri.clone(),
                ViewerImageRequest {
                    source: source.to_owned(),
                    generation,
                },
            );
        uri
    }

    pub fn release_viewer_uri(&self, uri: &str) {
        self.viewer_requests
            .write()
            .expect("viewer image request lock poisoned")
            .remove(uri);
    }

    fn viewer_request_is_live(&self, uri: &str, generation: u64) -> bool {
        self.generation.load(Ordering::Relaxed) == generation
            && self
                .viewer_requests
                .read()
                .expect("viewer image request lock poisoned")
                .contains_key(uri)
    }

    pub fn insert_mermaid_svg(&self, key: &str, svg: Arc<[u8]>) -> Result<(), String> {
        let mut store = self
            .mermaid_svgs
            .write()
            .expect("Mermaid SVG store lock poisoned");
        if store.entries.contains_key(key) {
            return Ok(());
        }
        if store.bytes.saturating_add(svg.len()) > MAX_MERMAID_SVG_STORE_BYTES {
            return Err(format!(
                "Mermaid SVG cache exceeds the {} MiB limit",
                MAX_MERMAID_SVG_STORE_BYTES / 1024 / 1024
            ));
        }
        store.bytes = store.bytes.saturating_add(svg.len());
        store.entries.insert(key.to_owned(), svg);
        Ok(())
    }

    pub fn retain_mermaid_svgs(&self, keys: &HashSet<String>) {
        let mut store = self
            .mermaid_svgs
            .write()
            .expect("Mermaid SVG store lock poisoned");
        store.entries.retain(|key, _| keys.contains(key));
        store.bytes = store.entries.values().map(|svg| svg.len()).sum();
    }
}

pub struct DocumentImageClient {
    document_root: DocumentImageRoot,
    remote_images: bool,
    fallback: Arc<dyn HttpClient>,
}

impl DocumentImageClient {
    pub fn new(
        document_root: DocumentImageRoot,
        remote_images: bool,
        fallback: Arc<dyn HttpClient>,
    ) -> Self {
        Self {
            document_root,
            remote_images,
            fallback,
        }
    }
}

impl HttpClient for DocumentImageClient {
    fn type_name(&self) -> &'static str {
        "native_markdown::DocumentImageClient"
    }

    fn user_agent(&self) -> Option<&HeaderValue> {
        self.fallback.user_agent()
    }

    fn proxy(&self) -> Option<&Url> {
        self.fallback.proxy()
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let uri = request.uri().to_string();
        if uri.starts_with(URI_PREFIX) {
            return mermaid_image_response(self.document_root.clone(), uri);
        }
        if self.remote_images {
            self.fallback.send(request)
        } else {
            let uri = request.uri().clone();
            Box::pin(async move { bail!("remote image is disabled: {uri}") })
        }
    }

    fn get(
        &self,
        uri: &str,
        body: AsyncBody,
        follow_redirects: bool,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        if uri.starts_with(VIEWER_URI_PREFIX) {
            return viewer_image_response(
                self.document_root.clone(),
                self.remote_images,
                self.fallback.clone(),
                uri.to_owned(),
                follow_redirects,
            );
        }
        if uri.starts_with(URI_PREFIX) {
            return mermaid_image_response(self.document_root.clone(), uri.to_owned());
        }
        let resolved = {
            let _document_guard = self
                .document_root
                .document_gate
                .lock()
                .expect("document image state lock poisoned");
            resolve_local_reference(&self.document_root, uri).map(|path| {
                path.map(|path| (path, self.document_root.generation.load(Ordering::Relaxed)))
            })
        };
        match resolved {
            Ok(Some((path, request_generation))) => {
                self.document_root
                    .requested_resources
                    .write()
                    .expect("requested image resource lock poisoned")
                    .insert(uri.to_owned());
                local_file_response(
                    path,
                    ImagePreparation {
                        max_width: self.document_root.viewport_width.load(Ordering::Relaxed),
                        max_pixels: u64::MAX,
                        retain: true,
                    },
                    request_generation,
                    self.document_root.generation.clone(),
                    self.document_root.preparation_gate.clone(),
                    self.document_root.prepared_images.clone(),
                    self.document_root.loads.clone(),
                )
            }
            Ok(None) if self.remote_images => self.fallback.get(uri, body, follow_redirects),
            Ok(None) => {
                let uri = uri.to_owned();
                Box::pin(async move { bail!("remote image is disabled: {uri}") })
            }
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}

fn viewer_image_response(
    root: DocumentImageRoot,
    remote_images: bool,
    fallback: Arc<dyn HttpClient>,
    request_uri: String,
    follow_redirects: bool,
) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
    Box::pin(async move {
        let request = root
            .viewer_requests
            .read()
            .expect("viewer image request lock poisoned")
            .get(&request_uri)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("image viewer request is no longer available"))?;
        if root.generation.load(Ordering::Relaxed) != request.generation {
            bail!("image viewer request belongs to an older document");
        }
        if request.source.starts_with(URI_PREFIX) {
            let response = mermaid_image_response_with_limits(
                root.clone(),
                request.source,
                VIEWER_MAX_IMAGE_WIDTH,
                VIEWER_MAX_IMAGE_PIXELS,
            )
            .await?;
            if !root.viewer_request_is_live(&request_uri, request.generation) {
                bail!("image viewer request was cancelled");
            }
            return Ok(response);
        }

        let local = {
            let _document_guard = root
                .document_gate
                .lock()
                .expect("document image state lock poisoned");
            resolve_local_reference(&root, &request.source)?
        };
        if let Some(path) = local {
            let response = local_file_response(
                path,
                ImagePreparation {
                    max_width: VIEWER_MAX_IMAGE_WIDTH,
                    max_pixels: VIEWER_MAX_IMAGE_PIXELS,
                    retain: false,
                },
                request.generation,
                root.generation.clone(),
                root.preparation_gate.clone(),
                root.prepared_images.clone(),
                root.loads.clone(),
            )
            .await?;
            if !root.viewer_request_is_live(&request_uri, request.generation) {
                bail!("image viewer request was cancelled");
            }
            return Ok(response);
        }
        if !remote_images {
            bail!("remote image is disabled");
        }

        let mut response = fallback
            .get(&request.source, AsyncBody::empty(), follow_redirects)
            .await?;
        let status = response.status();
        let mut bytes = Vec::new();
        response.body_mut().read_to_end(&mut bytes).await?;
        let bytes = prepare_image(bytes, VIEWER_MAX_IMAGE_WIDTH, VIEWER_MAX_IMAGE_PIXELS);
        if !root.viewer_request_is_live(&request_uri, request.generation) {
            bail!("image viewer request was cancelled");
        }
        Ok(Response::builder().status(status).body(bytes.into())?)
    })
}

fn mermaid_image_response(
    root: DocumentImageRoot,
    uri: String,
) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
    mermaid_image_response_with_limits(root, uri, MAX_RASTER_WIDTH, MAX_RASTER_PIXELS)
}

fn mermaid_image_response_with_limits(
    root: DocumentImageRoot,
    uri: String,
    max_width: u32,
    max_pixels: u64,
) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
    Box::pin(async move {
        let key = parse_mermaid_uri(&uri)?;
        let svg = root
            .mermaid_svgs
            .read()
            .expect("Mermaid SVG store lock poisoned")
            .entries
            .get(key)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Mermaid image is no longer available"))?;
        let png = rasterize_mermaid_svg(&svg, max_width, max_pixels)?;
        root.requested_resources
            .write()
            .expect("requested image resource lock poisoned")
            .insert(uri.clone());
        root.loads.fetch_add(1, Ordering::Relaxed);
        Ok(Response::builder()
            .status(StatusCode::OK)
            .body(png.into())?)
    })
}

fn parse_mermaid_uri(uri: &str) -> anyhow::Result<&str> {
    let remainder = uri
        .strip_prefix(URI_PREFIX)
        .ok_or_else(|| anyhow::anyhow!("invalid Mermaid image URI"))?;
    let key = remainder
        .strip_suffix(".png")
        .ok_or_else(|| anyhow::anyhow!("invalid Mermaid image extension"))?;
    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid Mermaid image key");
    }
    Ok(key)
}

fn rasterize_mermaid_svg(
    svg: &[u8],
    target_width: u32,
    max_pixels: u64,
) -> anyhow::Result<Vec<u8>> {
    static FONT_DATABASE: OnceLock<Arc<resvg::usvg::fontdb::Database>> = OnceLock::new();
    let fontdb = FONT_DATABASE.get_or_init(|| {
        let mut database = resvg::usvg::fontdb::Database::new();
        database.load_system_fonts();
        Arc::new(database)
    });
    let options = resvg::usvg::Options {
        font_family: "Segoe UI".to_owned(),
        fontdb: fontdb.clone(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(svg, &options)
        .map_err(|error| anyhow::anyhow!("invalid Mermaid SVG: {error}"))?;
    let source_size = tree.size();
    let source_width = source_size.width().max(1.0);
    let source_height = source_size.height().max(1.0);
    let desired_width = target_width.max(1) as f32;
    let mut scale = desired_width / source_width;
    let desired_pixels = source_width as f64 * source_height as f64 * scale as f64 * scale as f64;
    let pixel_limited = desired_pixels > max_pixels as f64;
    if pixel_limited {
        scale *= (max_pixels as f64 / desired_pixels).sqrt() as f32;
    }
    let round_dimension = |dimension: f32| {
        if pixel_limited {
            dimension.floor()
        } else {
            dimension.ceil()
        }
        .clamp(1.0, u32::MAX as f32) as u32
    };
    let width = round_dimension(source_width * scale);
    let height = round_dimension(source_height * scale);
    if pixel_limited {
        scale = (width as f32 / source_width).min(height as f32 / source_height);
    }
    if u64::from(width).saturating_mul(u64::from(height)) > max_pixels {
        bail!("Mermaid raster exceeds the {max_pixels} pixel limit");
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height)
        .ok_or_else(|| anyhow::anyhow!("could not allocate Mermaid raster"))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap
        .encode_png()
        .map_err(|error| anyhow::anyhow!("could not encode Mermaid raster: {error}"))
}

fn local_file_response(
    path: PathBuf,
    preparation: ImagePreparation,
    request_generation: u64,
    generation: Arc<AtomicU64>,
    preparation_gate: Arc<Mutex<()>>,
    prepared_images: PreparedImageCache,
    loads: Arc<AtomicUsize>,
) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
    Box::pin(async move {
        let ImagePreparation {
            max_width,
            max_pixels,
            retain,
        } = preparation;
        let bytes = smol::fs::read(&path)
            .await
            .with_context(|| format!("failed to read local image {}", path.display()))?;
        let cache_key = (path.clone(), max_width, max_pixels);
        let cached = if retain {
            prepared_images
                .lock()
                .expect("prepared image cache lock poisoned")
                .get(&cache_key)
                .cloned()
        } else {
            None
        };
        let bytes = if let Some(bytes) = cached {
            bytes
        } else {
            let _preparation_guard = preparation_gate
                .lock()
                .expect("image preparation gate lock poisoned");
            let cached_after_wait = if retain {
                prepared_images
                    .lock()
                    .expect("prepared image cache lock poisoned")
                    .get(&cache_key)
                    .cloned()
            } else {
                None
            };
            if let Some(bytes) = cached_after_wait {
                bytes
            } else {
                let bytes: Arc<[u8]> = prepare_image(bytes, max_width, max_pixels).into();
                if retain && generation.load(Ordering::Relaxed) == request_generation {
                    prepared_images
                        .lock()
                        .expect("prepared image cache lock poisoned")
                        .insert(cache_key, bytes.clone());
                }
                bytes
            }
        };
        if generation.load(Ordering::Relaxed) == request_generation {
            loads.fetch_add(1, Ordering::Relaxed);
        }
        Ok(Response::builder()
            .status(StatusCode::OK)
            .body(bytes.as_ref().to_vec().into())?)
    })
}

/// Downsize oversized local rasters before GPUI's global loader creates its retained BGRA buffer.
/// Unsupported and undecodable formats pass through byte-for-byte.
#[cfg(test)]
fn prepare_image_for_viewport(bytes: Vec<u8>, max_width: u32) -> Vec<u8> {
    prepare_image(bytes, max_width, u64::MAX)
}

fn prepare_image(bytes: Vec<u8>, max_width: u32, max_pixels: u64) -> Vec<u8> {
    let max_width = max_width.max(1);
    let Ok(format @ (ImageFormat::Png | ImageFormat::Jpeg)) = image::guess_format(&bytes) else {
        return bytes;
    };
    let Ok(image) = image::load_from_memory_with_format(&bytes, format) else {
        return bytes;
    };
    let source_pixels = u64::from(image.width()).saturating_mul(u64::from(image.height()));
    if image.width() <= max_width && source_pixels <= max_pixels {
        return bytes;
    }

    let width_scale = max_width as f64 / image.width().max(1) as f64;
    let pixel_scale = (max_pixels as f64 / source_pixels.max(1) as f64).sqrt();
    let scale = width_scale.min(pixel_scale).min(1.0);
    let target_width = (image.width() as f64 * scale).floor().max(1.0) as u32;
    let target_height = (image.height() as f64 * scale).floor().max(1.0) as u32;
    let resized = image.resize_exact(target_width, target_height, FilterType::Triangle);
    drop(image);
    let mut output = Cursor::new(Vec::new());
    if resized.write_to(&mut output, ImageFormat::Png).is_err() {
        return bytes;
    }
    output.into_inner()
}

fn resolve_local_reference(
    document_root: &DocumentImageRoot,
    reference: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let uri_path = reference
        .split_once(['?', '#'])
        .map_or(reference, |(path, _)| path);
    if uri_path.contains("://") || uri_path.starts_with('/') || uri_path.starts_with('\\') {
        return Ok(None);
    }
    if uri_path.is_empty() {
        return Ok(None);
    }

    let platform_path = uri_path.replace('/', std::path::MAIN_SEPARATOR_STR);
    let relative = Path::new(&platform_path);
    if relative.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        bail!("local image path escapes the document directory: {uri_path}");
    }

    let Some(root) = document_root
        .root
        .read()
        .expect("image root lock poisoned")
        .clone()
    else {
        bail!("local image has no document directory: {uri_path}");
    };
    let candidate = root.join(relative);
    let canonical_image = candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve local image {}", candidate.display()))?;
    if !canonical_image.starts_with(&root) {
        bail!("local image path escapes the document directory: {uri_path}");
    }
    Ok(Some(canonical_image))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_http_client::BlockedHttpClient;
    use image::{DynamicImage, ImageBuffer, Rgba};

    #[test]
    fn serves_relative_image_from_active_document_directory() {
        let directory = tempfile::tempdir().unwrap();
        let image_dir = directory.path().join("Image");
        std::fs::create_dir_all(&image_dir).unwrap();
        let expected = b"image bytes";
        std::fs::write(image_dir.join("figure.png"), expected).unwrap();
        let document_path = directory.path().join("chapter.md");
        std::fs::write(&document_path, "# Chapter").unwrap();

        let root = DocumentImageRoot::default();
        root.set_document_path(Some(&document_path));
        let client =
            DocumentImageClient::new(root.clone(), false, Arc::new(BlockedHttpClient::new()));
        let mut response =
            smol::block_on(client.get("Image/figure.png", AsyncBody::empty(), true)).unwrap();
        let mut actual = Vec::new();
        smol::block_on(response.body_mut().read_to_end(&mut actual)).unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(actual, expected);
        assert_eq!(root.load_count(), 1);
        assert_eq!(
            root.take_requested_resources(),
            vec!["Image/figure.png".to_owned()]
        );
        assert!(root.take_requested_resources().is_empty());
    }

    #[test]
    fn rejects_parent_directory_escape() {
        let directory = tempfile::tempdir().unwrap();
        let document_path = directory.path().join("chapter.md");
        std::fs::write(&document_path, "# Chapter").unwrap();
        let root = DocumentImageRoot::default();
        root.set_document_path(Some(&document_path));

        let error = resolve_local_reference(&root, "../secret.png").unwrap_err();
        assert!(error.to_string().contains("escapes"));
    }

    #[test]
    fn oversized_raster_is_prepared_at_viewport_width() {
        let source =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(300, 180, Rgba([1, 2, 3, 255])));
        let mut encoded = Cursor::new(Vec::new());
        source.write_to(&mut encoded, ImageFormat::Png).unwrap();

        let prepared = prepare_image_for_viewport(encoded.into_inner(), 120);
        let image = image::load_from_memory(&prepared).unwrap();

        assert_eq!(image.width(), 120);
        assert_eq!(image.height(), 72);
    }

    #[test]
    fn stale_image_request_cannot_repopulate_the_next_document_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.png");
        let source =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(300, 180, Rgba([1, 2, 3, 255])));
        let mut encoded = Cursor::new(Vec::new());
        source.write_to(&mut encoded, ImageFormat::Png).unwrap();
        std::fs::write(&path, encoded.into_inner()).unwrap();
        let generation = Arc::new(AtomicU64::new(2));
        let cache = PreparedImageCache::default();
        let loads = Arc::new(AtomicUsize::new(0));

        smol::block_on(local_file_response(
            path,
            ImagePreparation {
                max_width: 120,
                max_pixels: u64::MAX,
                retain: true,
            },
            1,
            generation,
            Arc::default(),
            cache.clone(),
            loads.clone(),
        ))
        .unwrap();

        assert!(cache.lock().unwrap().is_empty());
        assert_eq!(loads.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn viewer_raster_is_bounded_by_its_pixel_budget() {
        let source =
            DynamicImage::ImageRgba8(ImageBuffer::from_pixel(4_000, 3_000, Rgba([1, 2, 3, 255])));
        let mut encoded = Cursor::new(Vec::new());
        source.write_to(&mut encoded, ImageFormat::Png).unwrap();

        let prepared = prepare_image(encoded.into_inner(), VIEWER_MAX_IMAGE_WIDTH, 1_000_000);
        let image = image::load_from_memory(&prepared).unwrap();

        assert!(u64::from(image.width()) * u64::from(image.height()) <= 1_000_000);
        assert!((image.width() as f32 / image.height() as f32 - 4.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn document_switch_invalidates_viewer_resource() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("first.md");
        let second = directory.path().join("second.md");
        std::fs::write(&first, "# First").unwrap();
        std::fs::write(&second, "# Second").unwrap();
        let root = DocumentImageRoot::default();
        root.set_document_path(Some(&first));
        let uri = root.viewer_uri("figure.png");
        root.set_document_path(Some(&second));
        let client = DocumentImageClient::new(root, false, Arc::new(BlockedHttpClient::new()));

        let error = smol::block_on(client.get(&uri, AsyncBody::empty(), true))
            .err()
            .unwrap();

        assert!(error.to_string().contains("no longer available"));
    }

    #[test]
    fn viewer_local_image_does_not_retain_prepared_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let document = directory.path().join("images.md");
        let image_path = directory.path().join("figure.png");
        std::fs::write(&document, "![Figure](figure.png)").unwrap();
        DynamicImage::ImageRgba8(ImageBuffer::from_pixel(300, 180, Rgba([1, 2, 3, 255])))
            .save(&image_path)
            .unwrap();
        let root = DocumentImageRoot::default();
        root.set_document_path(Some(&document));
        let viewer_uri = root.viewer_uri("figure.png");
        let client =
            DocumentImageClient::new(root.clone(), false, Arc::new(BlockedHttpClient::new()));

        let mut response =
            smol::block_on(client.get(&viewer_uri, AsyncBody::empty(), true)).unwrap();
        let mut bytes = Vec::new();
        smol::block_on(response.body_mut().read_to_end(&mut bytes)).unwrap();

        assert!(!bytes.is_empty());
        assert!(root.prepared_images.lock().unwrap().is_empty());
    }

    #[test]
    fn serves_bounded_mermaid_png_without_network_fallback() {
        let root = DocumentImageRoot::default();
        let key = "a".repeat(64);
        let svg: Arc<[u8]> = Arc::from(
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="800" height="400"><rect width="800" height="400" fill="#587c8d"/></svg>"##
                .as_slice(),
        );
        root.insert_mermaid_svg(&key, svg).unwrap();
        let client =
            DocumentImageClient::new(root.clone(), true, Arc::new(BlockedHttpClient::new()));
        let uri = format!("{URI_PREFIX}{key}.png");
        let mut response = smol::block_on(client.get(&uri, AsyncBody::empty(), true)).unwrap();
        let mut bytes = Vec::new();
        smol::block_on(response.body_mut().read_to_end(&mut bytes)).unwrap();
        let image = image::load_from_memory(&bytes).unwrap();

        assert_eq!(image.width(), MAX_RASTER_WIDTH);
        assert_eq!(image.height(), MAX_RASTER_WIDTH / 2);
        assert_eq!(root.take_requested_resources(), vec![uri]);
    }

    #[test]
    fn viewer_mermaid_raster_rounding_stays_within_pixel_budget() {
        let root = DocumentImageRoot::default();
        let key = "c".repeat(64);
        let svg: Arc<[u8]> = Arc::from(
            br##"<svg xmlns="http://www.w3.org/2000/svg" width="3335.21875" height="536" viewBox="0 0 3335.21875 536"><rect width="3335.21875" height="536" fill="#587c8d"/></svg>"##
                .as_slice(),
        );
        root.insert_mermaid_svg(&key, svg).unwrap();
        let client =
            DocumentImageClient::new(root.clone(), false, Arc::new(BlockedHttpClient::new()));
        let source_uri = format!("{URI_PREFIX}{key}.png");
        let viewer_uri = root.viewer_uri(&source_uri);

        let mut response =
            smol::block_on(client.get(&viewer_uri, AsyncBody::empty(), true)).unwrap();
        let mut bytes = Vec::new();
        smol::block_on(response.body_mut().read_to_end(&mut bytes)).unwrap();
        let image = image::load_from_memory(&bytes).unwrap();

        assert!(u64::from(image.width()) * u64::from(image.height()) <= VIEWER_MAX_IMAGE_PIXELS);
    }

    #[test]
    fn unknown_mermaid_key_never_falls_back_to_remote_http() {
        let root = DocumentImageRoot::default();
        let client = DocumentImageClient::new(root, true, Arc::new(BlockedHttpClient::new()));
        let uri = format!("{URI_PREFIX}{}.png", "b".repeat(64));
        let error = smol::block_on(client.get(&uri, AsyncBody::empty(), true))
            .err()
            .unwrap();
        assert!(error.to_string().contains("no longer available"));
    }
}
