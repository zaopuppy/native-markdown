use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};

use anyhow::{bail, Context as _};
use futures::future::BoxFuture;
use gpui_http_client::http::HeaderValue;
use gpui_http_client::{AsyncBody, HttpClient, Request, Response, StatusCode, Url};

#[derive(Clone, Default)]
pub struct DocumentImageRoot {
    root: Arc<RwLock<Option<PathBuf>>>,
    loads: Arc<AtomicUsize>,
}

impl DocumentImageRoot {
    pub fn set_document_path(&self, path: Option<&Path>) {
        let root = path
            .and_then(Path::parent)
            .and_then(|path| path.canonicalize().ok());
        *self.root.write().expect("image root lock poisoned") = root;
        self.loads.store(0, Ordering::Relaxed);
    }

    pub fn load_count(&self) -> usize {
        self.loads.load(Ordering::Relaxed)
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
        match resolve_local_reference(&self.document_root, uri) {
            Ok(Some(path)) => local_file_response(path, self.document_root.loads.clone()),
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
    loads: Arc<AtomicUsize>,
) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
    Box::pin(async move {
        let bytes = smol::fs::read(&path)
            .await
            .with_context(|| format!("failed to read local image {}", path.display()))?;
        loads.fetch_add(1, Ordering::Relaxed);
        Ok(Response::builder()
            .status(StatusCode::OK)
            .body(bytes.into())?)
    })
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
}
