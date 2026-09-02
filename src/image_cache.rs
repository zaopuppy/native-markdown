use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::FutureExt as _;
use gpui::{
    hash, App, AppContext, Asset, AssetLogger, Entity, ImageCache, ImageCacheError, ImageCacheItem,
    ImgResourceLoader, RenderImage, Resource, Window,
};
use image::{imageops::FilterType, Frame, ImageBuffer, Rgba};

use crate::image_loader::{VIEWER_MAX_IMAGE_PIXELS, VIEWER_MAX_IMAGE_WIDTH, VIEWER_URI_PREFIX};
use crate::mermaid::{MAX_RASTER_WIDTH, URI_PREFIX};

pub const SOFT_BUDGET_BYTES: usize = 100 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageCacheStatus {
    pub estimated_bytes: usize,
    pub over_soft_budget: bool,
}

struct CacheEntry {
    resource: Resource,
    item: ImageCacheItem,
    estimated_bytes: Option<usize>,
    last_access: u64,
}

/// A retained document-scoped image cache with an explicit CPU + GPU memory estimate.
///
/// GPUI exposes decoded BGRA frames but not renderer allocation sizes, so the estimate
/// counts every decoded frame twice: once for the CPU buffer and once for its texture.
pub struct BudgetImageCache {
    entries: HashMap<u64, CacheEntry>,
    protected: HashSet<u64>,
    estimated_bytes: usize,
    access_clock: u64,
}

impl BudgetImageCache {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|_| Self {
            entries: HashMap::new(),
            protected: HashSet::new(),
            estimated_bytes: 0,
            access_clock: 0,
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
            over_soft_budget: self.estimated_bytes > SOFT_BUDGET_BYTES,
        }
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut App) {
        let hashes = self.entries.keys().copied().collect::<Vec<_>>();
        for hash in hashes {
            self.remove(hash, window, cx);
        }
        self.protected.clear();
    }

    pub fn protect_resource(&mut self, resource: &Resource) {
        self.protected.insert(hash(resource));
    }

    pub fn release_resource(&mut self, resource: &Resource, window: &mut Window, cx: &mut App) {
        let hash = hash(resource);
        self.protected.remove(&hash);
        self.remove(hash, window, cx);
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
        self.protected.remove(&hash);
        let Some(mut entry) = self.entries.remove(&hash) else {
            return;
        };
        self.estimated_bytes = self
            .estimated_bytes
            .saturating_sub(entry.estimated_bytes.unwrap_or(0));
        let cached_image = entry.item.get().and_then(Result::ok);
        if let Some(image) = &cached_image {
            cx.drop_image(image.clone(), Some(window));
        }
        if let Some(Ok(global_image)) = window.get_asset::<ImgResourceLoader>(&entry.resource, cx) {
            let already_dropped = cached_image
                .as_ref()
                .is_some_and(|cached| Arc::ptr_eq(cached, &global_image));
            if !already_dropped {
                cx.drop_image(global_image, Some(window));
            }
        }
        cx.remove_asset::<ImgResourceLoader>(&entry.resource);
    }

    fn evict_to_budget(&mut self, protected_hash: u64, window: &mut Window, cx: &mut App) {
        while self.estimated_bytes > SOFT_BUDGET_BYTES {
            let Some(candidate) = select_lru_candidate(
                self.entries.iter().map(|(hash, entry)| {
                    (*hash, entry.last_access, entry.estimated_bytes.unwrap_or(0))
                }),
                protected_hash,
                &self.protected,
            ) else {
                break;
            };
            self.remove(candidate, window, cx);
        }
    }
}

fn select_lru_candidate(
    entries: impl IntoIterator<Item = (u64, u64, usize)>,
    protected_hash: u64,
    protected: &HashSet<u64>,
) -> Option<u64> {
    entries
        .into_iter()
        .filter(|(hash, _, bytes)| {
            *hash != protected_hash && !protected.contains(hash) && *bytes > 0
        })
        .min_by_key(|(_, last_access, _)| *last_access)
        .map(|(hash, _, _)| hash)
}

#[derive(Clone, Hash)]
struct SizedImageResource {
    resource: Resource,
    max_width: u32,
    max_pixels: u64,
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
            downsample_to_limits(image, source.max_width, source.max_pixels)
        }
    }
}

fn viewport_device_width(window: &Window) -> u32 {
    let logical_width: f32 = window.viewport_size().width.into();
    (logical_width * window.scale_factor())
        .ceil()
        .clamp(1.0, u32::MAX as f32) as u32
}

#[cfg(test)]
fn scaled_dimensions(width: u32, height: u32, max_width: u32) -> Option<(u32, u32)> {
    scaled_dimensions_with_limit(width, height, max_width, u64::MAX)
}

fn scaled_dimensions_with_limit(
    width: u32,
    height: u32,
    max_width: u32,
    max_pixels: u64,
) -> Option<(u32, u32)> {
    if width == 0 || height == 0 {
        return None;
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if width <= max_width && pixels <= max_pixels {
        return None;
    }
    let width_scale = max_width as f64 / width as f64;
    let pixel_scale = (max_pixels as f64 / pixels.max(1) as f64).sqrt();
    let scale = width_scale.min(pixel_scale).min(1.0);
    Some((
        (width as f64 * scale).floor().max(1.0) as u32,
        (height as f64 * scale).floor().max(1.0) as u32,
    ))
}

#[cfg(test)]
fn downsample_to_width(
    image: Arc<RenderImage>,
    max_width: u32,
) -> Result<Arc<RenderImage>, ImageCacheError> {
    downsample_to_limits(image, max_width, u64::MAX)
}

fn downsample_to_limits(
    image: Arc<RenderImage>,
    max_width: u32,
    max_pixels: u64,
) -> Result<Arc<RenderImage>, ImageCacheError> {
    let needs_resize = (0..image.frame_count()).any(|frame_index| {
        let size = image.size(frame_index);
        let Ok(width) = u32::try_from(i32::from(size.width)) else {
            return true;
        };
        let Ok(height) = u32::try_from(i32::from(size.height)) else {
            return true;
        };
        width > max_width || u64::from(width).saturating_mul(u64::from(height)) > max_pixels
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
        let buffer = match scaled_dimensions_with_limit(width, height, max_width, max_pixels) {
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
        self.access_clock = self.access_clock.wrapping_add(1);
        let access = self.access_clock;

        if let Some(entry) = self.entries.get_mut(&resource_hash) {
            entry.last_access = access;
            let result = entry.item.get()?;
            self.record_loaded_size(resource_hash, &result);
            self.evict_to_budget(resource_hash, window, cx);
            return Some(result);
        }

        let (max_width, max_pixels) = match resource {
            Resource::Uri(uri) if uri.as_ref().starts_with(VIEWER_URI_PREFIX) => {
                (VIEWER_MAX_IMAGE_WIDTH, VIEWER_MAX_IMAGE_PIXELS)
            }
            Resource::Uri(uri) if uri.as_ref().starts_with(URI_PREFIX) => {
                (MAX_RASTER_WIDTH, u64::MAX)
            }
            _ => (viewport_device_width(window), u64::MAX),
        };

        let future = AssetLogger::<SizedImageAssetLoader>::load(
            SizedImageResource {
                resource: resource.clone(),
                max_width,
                max_pixels,
            },
            cx,
        );
        let task = cx.background_executor().spawn(future).shared();
        self.entries.insert(
            resource_hash,
            CacheEntry {
                resource: resource.clone(),
                item: ImageCacheItem::Loading(task.clone()),
                estimated_bytes: None,
                last_access: access,
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
    fn image_cache_soft_budget_is_one_hundred_mib() {
        assert_eq!(SOFT_BUDGET_BYTES / 1024 / 1024, 100);
    }

    #[test]
    fn lru_eviction_prefers_the_oldest_loaded_unprotected_candidate() {
        let entries = [(10, 1, 32), (20, 2, 48), (30, 0, 0), (40, 4, 64)];

        assert_eq!(select_lru_candidate(entries, 20, &HashSet::new()), Some(10));
        assert_eq!(select_lru_candidate(entries, 10, &HashSet::new()), Some(20));
        assert_eq!(
            select_lru_candidate(entries, 30, &HashSet::from([10])),
            Some(20)
        );
    }

    #[test]
    fn large_images_are_scaled_to_the_display_width() {
        assert_eq!(scaled_dimensions(3000, 1800, 1200), Some((1200, 720)));
        assert_eq!(scaled_dimensions(800, 600, 1200), None);
    }

    #[test]
    fn viewer_dimensions_respect_pixel_budget() {
        let dimensions = scaled_dimensions_with_limit(4_000, 3_000, 8_192, 1_000_000).unwrap();
        assert!(u64::from(dimensions.0) * u64::from(dimensions.1) <= 1_000_000);
        assert!((dimensions.0 as f32 / dimensions.1 as f32 - 4.0 / 3.0).abs() < 0.01);
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
