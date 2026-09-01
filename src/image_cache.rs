use std::collections::HashMap;
use std::sync::Arc;

use futures::FutureExt as _;
use gpui::{
    hash, App, AppContext, Asset, AssetLogger, Entity, ImageCache, ImageCacheError, ImageCacheItem,
    ImgResourceLoader, RenderImage, Resource, Window,
};
use image::{imageops::FilterType, Frame, ImageBuffer, Rgba};

pub const WARNING_THRESHOLD_BYTES: usize = 48 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageCacheStatus {
    pub estimated_bytes: usize,
    pub over_warning_threshold: bool,
}

struct CacheEntry {
    item: ImageCacheItem,
    estimated_bytes: Option<usize>,
}

/// A retained document-scoped image cache with an explicit CPU + GPU memory estimate.
///
/// GPUI exposes decoded BGRA frames but not renderer allocation sizes, so the estimate
/// counts every decoded frame twice: once for the CPU buffer and once for its texture.
pub struct BudgetImageCache {
    entries: HashMap<u64, CacheEntry>,
    estimated_bytes: usize,
}

impl BudgetImageCache {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|_| Self {
            entries: HashMap::new(),
            estimated_bytes: 0,
        });
        cx.observe_release(&cache, |cache, cx| {
            for (_, mut entry) in std::mem::take(&mut cache.entries) {
                if let Some(Ok(image)) = entry.item.get() {
                    cx.drop_image(image, None);
                }
            }
        })
        .detach();
        cache
    }

    pub fn status(&self) -> ImageCacheStatus {
        ImageCacheStatus {
            estimated_bytes: self.estimated_bytes,
            over_warning_threshold: self.estimated_bytes > WARNING_THRESHOLD_BYTES,
        }
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut App) {
        let hashes = self.entries.keys().copied().collect::<Vec<_>>();
        for hash in hashes {
            self.remove(hash, window, cx);
        }
    }

    pub fn remove_resource(&mut self, resource: &Resource, window: &mut Window, cx: &mut App) {
        self.remove(hash(resource), window, cx);
    }

    fn record_loaded_size(
        &mut self,
        hash: u64,
        result: &Result<Arc<RenderImage>, ImageCacheError>,
    ) {
        let Some(entry) = self.entries.get_mut(&hash) else {
            return;
        };
        if entry.estimated_bytes.is_some() {
            return;
        }

        let bytes = result.as_ref().map_or(0, |image| {
            (0..image.frame_count())
                .filter_map(|frame| image.as_bytes(frame).map(<[u8]>::len))
                .fold(0usize, usize::saturating_add)
                .saturating_mul(2)
        });
        entry.estimated_bytes = Some(bytes);
        self.estimated_bytes = self.estimated_bytes.saturating_add(bytes);
    }

    fn remove(&mut self, hash: u64, window: &mut Window, cx: &mut App) {
        let Some(mut entry) = self.entries.remove(&hash) else {
            return;
        };
        self.estimated_bytes = self
            .estimated_bytes
            .saturating_sub(entry.estimated_bytes.unwrap_or(0));
        if let Some(Ok(image)) = entry.item.get() {
            cx.drop_image(image, Some(window));
        }
    }
}

#[derive(Clone, Hash)]
struct SizedImageResource {
    resource: Resource,
    max_width: u32,
}

enum SizedImageAssetLoader {}

impl Asset for SizedImageAssetLoader {
    type Source = SizedImageResource;
    type Output = Result<Arc<RenderImage>, ImageCacheError>;

    fn load(
        source: Self::Source,
        cx: &mut App,
    ) -> impl std::future::Future<Output = Self::Output> + Send + 'static {
        // TextView's virtual list can request the same image through GPUI's global loader before
        // the ancestor ImageCache is in scope. Share that task instead of decoding a second copy.
        let (image, _) = cx.fetch_asset::<ImgResourceLoader>(&source.resource);
        async move {
            let image = image.await?;
            downsample_to_width(image, source.max_width)
        }
    }
}

fn viewport_device_width(window: &Window) -> u32 {
    let logical_width: f32 = window.viewport_size().width.into();
    (logical_width * window.scale_factor())
        .ceil()
        .clamp(1.0, u32::MAX as f32) as u32
}

fn scaled_dimensions(width: u32, height: u32, max_width: u32) -> Option<(u32, u32)> {
    if width <= max_width || width == 0 || height == 0 {
        return None;
    }

    let target_height = ((height as u64 * max_width as u64) / width as u64)
        .max(1)
        .min(u32::MAX as u64) as u32;
    Some((max_width, target_height))
}

fn downsample_to_width(
    image: Arc<RenderImage>,
    max_width: u32,
) -> Result<Arc<RenderImage>, ImageCacheError> {
    let needs_resize = (0..image.frame_count()).any(|frame_index| {
        let size = image.size(frame_index);
        u32::try_from(i32::from(size.width)).is_ok_and(|width| width > max_width)
    });
    if !needs_resize {
        return Ok(image);
    }

    let mut frames = Vec::with_capacity(image.frame_count());
    for frame_index in 0..image.frame_count() {
        let size = image.size(frame_index);
        let width = u32::try_from(i32::from(size.width)).map_err(|_| {
            ImageCacheError::Asset("image width cannot be represented as u32".into())
        })?;
        let height = u32::try_from(i32::from(size.height)).map_err(|_| {
            ImageCacheError::Asset("image height cannot be represented as u32".into())
        })?;
        let bytes = image
            .as_bytes(frame_index)
            .ok_or_else(|| ImageCacheError::Asset("image frame has no pixel data".into()))?;
        let buffer = ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(width, height, bytes.to_vec())
            .ok_or_else(|| ImageCacheError::Asset("image frame has an invalid size".into()))?;
        let buffer = match scaled_dimensions(width, height, max_width) {
            Some((target_width, target_height)) => {
                image::imageops::resize(&buffer, target_width, target_height, FilterType::Lanczos3)
            }
            None => buffer,
        };
        frames.push(Frame::from_parts(buffer, 0, 0, image.delay(frame_index)));
    }

    Ok(Arc::new(RenderImage::new(frames)))
}

impl ImageCache for BudgetImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let resource_hash = hash(resource);

        if let Some(entry) = self.entries.get_mut(&resource_hash) {
            let result = entry.item.get()?;
            self.record_loaded_size(resource_hash, &result);
            return Some(result);
        }

        let future = AssetLogger::<SizedImageAssetLoader>::load(
            SizedImageResource {
                resource: resource.clone(),
                max_width: viewport_device_width(window),
            },
            cx,
        );
        let task = cx.background_executor().spawn(future).shared();
        self.entries.insert(
            resource_hash,
            CacheEntry {
                item: ImageCacheItem::Loading(task.clone()),
                estimated_bytes: None,
            },
        );

        let entity = window.current_view();
        window
            .spawn(cx, async move |cx| {
                let _ = task.await;
                cx.on_next_frame(move |_, cx| cx.notify(entity));
            })
            .detach();
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warning_threshold_does_not_imply_an_eviction_limit() {
        assert_eq!(WARNING_THRESHOLD_BYTES / 1024 / 1024, 48);
    }

    #[test]
    fn large_images_are_scaled_to_the_display_width() {
        assert_eq!(scaled_dimensions(3000, 1800, 1200), Some((1200, 720)));
        assert_eq!(scaled_dimensions(800, 600, 1200), None);
    }

    #[test]
    fn downsampling_preserves_aspect_ratio_and_reduces_pixel_storage() {
        let pixels = ImageBuffer::from_pixel(300, 180, Rgba([1, 2, 3, 255]));
        let original = Arc::new(RenderImage::new(vec![Frame::new(pixels)]));

        let resized = downsample_to_width(original, 120).unwrap();

        let size = resized.size(0);
        assert_eq!(i32::from(size.width), 120);
        assert_eq!(i32::from(size.height), 72);
        assert_eq!(resized.as_bytes(0).unwrap().len(), 120 * 72 * 4);
    }
}
