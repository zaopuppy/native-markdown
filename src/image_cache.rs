use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::FutureExt as _;
use gpui::{
    hash, App, AppContext, Asset, AssetLogger, Entity, ImageAssetLoader, ImageCache,
    ImageCacheError, ImageCacheItem, RenderImage, Resource, Window,
};

pub const SOFT_BUDGET_BYTES: usize = 48 * 1024 * 1024;
pub const HARD_BUDGET_BYTES: usize = 160 * 1024 * 1024;
const IDLE_EVICTION_DELAY: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageCacheStatus {
    pub estimated_bytes: usize,
    pub over_soft_budget: bool,
}

struct CacheEntry {
    item: ImageCacheItem,
    estimated_bytes: Option<usize>,
    last_used: u64,
}

/// A document-scoped image cache with an explicit decoded CPU + GPU memory estimate.
///
/// GPUI exposes decoded BGRA frames but not renderer allocation sizes, so the budget
/// counts every decoded frame twice: once for the CPU buffer and once for its texture.
pub struct BudgetImageCache {
    entries: HashMap<u64, CacheEntry>,
    estimated_bytes: usize,
    clock: u64,
    last_scroll: Option<Instant>,
}

impl BudgetImageCache {
    pub fn new(cx: &mut App) -> Entity<Self> {
        let cache = cx.new(|_| Self {
            entries: HashMap::new(),
            estimated_bytes: 0,
            clock: 0,
            last_scroll: None,
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

    pub fn note_scroll(&mut self) {
        self.last_scroll = Some(Instant::now());
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
        self.last_scroll = None;
    }

    pub fn trim_if_idle(&mut self, window: &mut Window, cx: &mut App) {
        if self.estimated_bytes <= SOFT_BUDGET_BYTES || self.is_scrolling() {
            return;
        }
        self.trim_to(SOFT_BUDGET_BYTES, None, window, cx);
    }

    fn is_scrolling(&self) -> bool {
        self.last_scroll
            .is_some_and(|last_scroll| last_scroll.elapsed() < IDLE_EVICTION_DELAY)
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

    fn enforce_budgets(&mut self, current_hash: u64, window: &mut Window, cx: &mut App) -> bool {
        if self.estimated_bytes > HARD_BUDGET_BYTES {
            self.trim_to(HARD_BUDGET_BYTES, Some(current_hash), window, cx);
            if self.estimated_bytes > HARD_BUDGET_BYTES {
                self.remove(current_hash, window, cx);
                return false;
            }
        }

        if self.estimated_bytes > SOFT_BUDGET_BYTES && !self.is_scrolling() {
            self.trim_to(SOFT_BUDGET_BYTES, Some(current_hash), window, cx);
        }
        true
    }

    fn trim_to(
        &mut self,
        budget: usize,
        protected: Option<u64>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let mut candidates = self
            .entries
            .iter()
            .filter(|(hash, entry)| Some(**hash) != protected && entry.estimated_bytes.is_some())
            .map(|(hash, entry)| (*hash, entry.last_used))
            .collect::<Vec<_>>();
        candidates.sort_unstable_by_key(|(_, last_used)| *last_used);

        for (hash, _) in candidates {
            if self.estimated_bytes <= budget || self.entries.len() <= 1 {
                break;
            }
            self.remove(hash, window, cx);
        }
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

impl ImageCache for BudgetImageCache {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let resource_hash = hash(resource);
        self.clock = self.clock.wrapping_add(1);

        if let Some(entry) = self.entries.get_mut(&resource_hash) {
            entry.last_used = self.clock;
            let result = entry.item.get()?;
            self.record_loaded_size(resource_hash, &result);
            if !self.enforce_budgets(resource_hash, window, cx) {
                return Some(Err(ImageCacheError::Asset(
                    "image exceeds the 160 MiB cache limit".into(),
                )));
            }
            return Some(result);
        }

        let future = AssetLogger::<ImageAssetLoader>::load(resource.clone(), cx);
        let task = cx.background_executor().spawn(future).shared();
        self.entries.insert(
            resource_hash,
            CacheEntry {
                item: ImageCacheItem::Loading(task.clone()),
                estimated_bytes: None,
                last_used: self.clock,
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
    fn published_budgets_match_reader_policy() {
        assert_eq!(SOFT_BUDGET_BYTES / 1024 / 1024, 48);
        assert_eq!(HARD_BUDGET_BYTES / 1024 / 1024, 160);
        assert!(HARD_BUDGET_BYTES > SOFT_BUDGET_BYTES);
    }
}
