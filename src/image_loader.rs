use std::collections::{HashMap, HashSet};
use std::io::Cursor;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use anyhow::{bail, Context as _};
use futures::future::BoxFuture;
use gpui_http_client::http::HeaderValue;
use gpui_http_client::{AsyncBody, HttpClient, Request, Response, StatusCode, Url};
use image::{imageops::FilterType, ImageFormat};

// Markdown images never need more decoded pixels than the reading surface can display. A bounded
// physical width also keeps ultra-wide source figures from dominating the process heap.
const MAX_PREPARED_IMAGE_WIDTH: u32 = 1_280;
type PreparedImageCache = Arc<Mutex<HashMap<(PathBuf, u32), Arc<[u8]>>>>;

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
                    self.document_root.viewport_width.load(Ordering::Relaxed),
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

fn local_file_response(
    path: PathBuf,
    max_width: u32,
    request_generation: u64,
    generation: Arc<AtomicU64>,
    preparation_gate: Arc<Mutex<()>>,
    prepared_images: PreparedImageCache,
    loads: Arc<AtomicUsize>,
) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
    Box::pin(async move {
        let bytes = smol::fs::read(&path)
            .await
            .with_context(|| format!("failed to read local image {}", path.display()))?;
        let cache_key = (path.clone(), max_width);
        let cached = prepared_images
            .lock()
            .expect("prepared image cache lock poisoned")
            .get(&cache_key)
            .cloned();
        let bytes = if let Some(bytes) = cached {
            bytes
        } else {
            let _preparation_guard = preparation_gate
                .lock()
                .expect("image preparation gate lock poisoned");
            let cached_after_wait = prepared_images
                .lock()
                .expect("prepared image cache lock poisoned")
                .get(&cache_key)
                .cloned();
            if let Some(bytes) = cached_after_wait {
                bytes
            } else {
                let bytes: Arc<[u8]> = prepare_image_for_viewport(bytes, max_width).into();
                if generation.load(Ordering::Relaxed) == request_generation {
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
fn prepare_image_for_viewport(bytes: Vec<u8>, max_width: u32) -> Vec<u8> {
    let max_width = max_width.max(1);
    let Ok(format @ (ImageFormat::Png | ImageFormat::Jpeg)) = image::guess_format(&bytes) else {
        return bytes;
    };
    let Ok(image) = image::load_from_memory_with_format(&bytes, format) else {
        return bytes;
    };
    if image.width() <= max_width {
        return bytes;
    }

    let resized = image.resize(max_width, u32::MAX, FilterType::Triangle);
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
    use futures::AsyncReadExt as _;
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
            120,
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
}
