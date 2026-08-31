use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context as _, bail};
use futures::future::BoxFuture;
use gpui_http_client::http::HeaderValue;
use gpui_http_client::{AsyncBody, HttpClient, Request, Response, StatusCode, Url};

static LOCAL_IMAGE_LOADS: AtomicUsize = AtomicUsize::new(0);

pub struct LocalImageHttpClient {
    base_dir: PathBuf,
    remote_images: bool,
    fallback: Arc<dyn HttpClient>,
}

impl LocalImageHttpClient {
    pub fn new(base_dir: PathBuf, remote_images: bool, fallback: Arc<dyn HttpClient>) -> Self {
        Self {
            base_dir,
            remote_images,
            fallback,
        }
    }
}

impl HttpClient for LocalImageHttpClient {
    fn type_name(&self) -> &'static str {
        "markdown_spike::LocalImageHttpClient"
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
        match resolve_local_reference(&self.base_dir, uri) {
            Ok(Some(path)) => local_file_response(path),
            Ok(None) if self.remote_images => self.fallback.get(uri, body, follow_redirects),
            Ok(None) => {
                let uri = uri.to_owned();
                Box::pin(async move { bail!("remote image is disabled: {uri}") })
            }
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}

pub fn local_image_load_count() -> usize {
    LOCAL_IMAGE_LOADS.load(Ordering::Relaxed)
}

fn local_file_response(path: PathBuf) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
    Box::pin(async move {
        let bytes = smol::fs::read(&path)
            .await
            .with_context(|| format!("failed to read local image {}", path.display()))?;
        LOCAL_IMAGE_LOADS.fetch_add(1, Ordering::Relaxed);
        Ok(Response::builder()
            .status(StatusCode::OK)
            .body(bytes.into())?)
    })
}

fn resolve_local_reference(base_dir: &Path, reference: &str) -> anyhow::Result<Option<PathBuf>> {
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

    let canonical_base = base_dir.canonicalize().with_context(|| {
        format!(
            "failed to resolve document directory {}",
            base_dir.display()
        )
    })?;
    let candidate = canonical_base.join(relative);
    let canonical_image = candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve local image {}", candidate.display()))?;
    if !canonical_image.starts_with(&canonical_base) {
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
    fn serves_relative_image_from_document_directory() {
        let directory = tempfile::tempdir().unwrap();
        let image_dir = directory.path().join("Image").join("chapter1");
        std::fs::create_dir_all(&image_dir).unwrap();
        let expected = b"not-a-real-png-but-the-loader-boundary-does-not-care";
        std::fs::write(image_dir.join("figure1.1.png"), expected).unwrap();

        let client = LocalImageHttpClient::new(
            directory.path().to_path_buf(),
            false,
            Arc::new(BlockedHttpClient::new()),
        );
        let mut response =
            smol::block_on(client.get("Image/chapter1/figure1.1.png", AsyncBody::empty(), true))
                .unwrap();
        let mut actual = Vec::new();
        smol::block_on(response.body_mut().read_to_end(&mut actual)).unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(actual, expected);
    }

    #[test]
    fn blocks_parent_directory_escape() {
        let directory = tempfile::tempdir().unwrap();
        let error = resolve_local_reference(directory.path(), "../secret.png").unwrap_err();
        assert!(error.to_string().contains("escapes"));
    }

    #[test]
    fn leaves_remote_uri_for_policy_handling() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            resolve_local_reference(directory.path(), "https://example.com/image.png")
                .unwrap()
                .is_none()
        );
    }
}
