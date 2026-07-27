//! In-RAM, byte-budgeted LRU cache of SQL results, plus the machinery that
//! makes it safe to use on the query hot path.
//!
//! ## What is cached
//!
//! Read-only SELECTs on the query hot path whose results are a pure function
//! of (persistent rows, rows of eph layers in the current chain).  Explicitly
//! excluded: populate-internal queries (they run inside an [`EphTransaction`]
//! on a layer-cache miss — at most once per layer lifetime — and must see
//! uncommitted transaction state, which a shared cache must never serve; the
//! structural opt-out is that they run on `txn.connection()` while the cache
//! lives behind `Index::cached_load`), `get_file_contents` (large payloads),
//! index_store admin queries, and all writes.
//!
//! ## Keying
//!
//! The cache key is the SHA-256 of the *rendered* SQL plus binds
//! (`diesel::debug_query`), combined with the row `TypeId`.  Keys are derived
//! from the final query object, not from hand-enumerated parameters, so a
//! future query cannot be under-keyed by forgetting an input.  Rendering may
//! change across diesel upgrades — that silently re-keys everything, which is
//! benign for a RAM-only cache (equivalent to a cold restart).
//!
//! ## Invalidation
//!
//! Single-instance askld is assumed.  The mutation paths (`finalize_project`,
//! `delete_project`) call [`SqlResultCache::clear`] after their transaction
//! commits.  The `epoch` counter closes the race with in-flight loads: a load
//! snapshots the epoch BEFORE its DB read; `clear()` bumps the epoch; a put
//! whose snapshot is stale is rejected, so a pre-mutation read can never be
//! inserted after the clear.  Background eph-layer GC needs no clear: chain
//! ids are never reused, so entries keyed with dead chain ids can never be
//! requested again and simply age out via LRU.

use std::any::{Any, TypeId};
use std::collections::Bound;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use diesel::pg::Pg;
use diesel::query_builder::QueryFragment;
use sha2::{Digest, Sha256};

// ============================================================================
// Cache key
// ============================================================================

/// Cache key: hash of the rendered SQL+binds, plus the row type.
///
/// The `TypeId` component separates two queries that render identical SQL but
/// deserialize into different row types (e.g. a narrowed select) — without
/// it, a downcast on hit would fail closed, but keeping the types apart also
/// keeps the accounting honest.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct CacheKey {
    sql_hash: [u8; 32],
    type_id: TypeId,
}

impl CacheKey {
    /// Render the query (SQL text + binds, no connection needed) and hash it.
    /// Returns `None` when the query cannot be rendered (a `walk_ast` error
    /// surfaces as `fmt::Error`); the caller then degrades to an uncached
    /// load through the same code path.
    pub fn for_query<T: 'static, Q: QueryFragment<Pg>>(query: &Q) -> Option<Self> {
        use std::fmt::Write;

        /// Streams the Debug rendering straight into the hasher — no
        /// intermediate String.  Bind arrays can carry thousands of ids,
        /// and this runs on EVERY load including cache hits, so the key
        /// computation must not allocate proportionally to the query.
        struct HashWriter(Sha256);
        impl std::fmt::Write for HashWriter {
            fn write_str(&mut self, s: &str) -> std::fmt::Result {
                self.0.update(s.as_bytes());
                Ok(())
            }
        }

        let mut hw = HashWriter(Sha256::new());
        write!(&mut hw, "{:?}", diesel::debug_query::<Pg, _>(query)).ok()?;
        Some(Self {
            sql_hash: hw.0.finalize().into(),
            type_id: TypeId::of::<T>(),
        })
    }

    #[cfg(test)]
    pub fn for_test<T: 'static>(tag: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(tag);
        Self {
            sql_hash: hasher.finalize().into(),
            type_id: TypeId::of::<T>(),
        }
    }
}

// ============================================================================
// Size accounting
// ============================================================================

/// Extra heap bytes owned by a value, beyond its `size_of` footprint.
/// [`vec_weight`] combines this with the inline sizes to estimate a cached
/// entry's total bytes.  The estimate deliberately ignores allocator slack
/// and shared-pointer overhead — the budget is a soft bound.
pub trait CacheWeight {
    fn heap_bytes(&self) -> usize;
}

macro_rules! zero_heap {
    ($($t:ty),* $(,)?) => {
        $(impl CacheWeight for $t {
            fn heap_bytes(&self) -> usize { 0 }
        })*
    };
}
zero_heap!(i8, i16, i32, i64, u8, u16, u32, u64, usize, bool, f32, f64);

impl CacheWeight for String {
    fn heap_bytes(&self) -> usize {
        self.capacity()
    }
}

impl<T: CacheWeight> CacheWeight for Option<T> {
    fn heap_bytes(&self) -> usize {
        self.as_ref().map_or(0, CacheWeight::heap_bytes)
    }
}

impl<T> CacheWeight for (Bound<T>, Bound<T>) {
    fn heap_bytes(&self) -> usize {
        0
    }
}

impl<T: CacheWeight> CacheWeight for Vec<T> {
    fn heap_bytes(&self) -> usize {
        self.capacity() * std::mem::size_of::<T>()
            + self.iter().map(CacheWeight::heap_bytes).sum::<usize>()
    }
}

macro_rules! tuple_weight {
    ($($name:ident : $idx:tt),+) => {
        impl<$($name: CacheWeight),+> CacheWeight for ($($name,)+) {
            fn heap_bytes(&self) -> usize {
                0 $(+ self.$idx.heap_bytes())+
            }
        }
    };
}
tuple_weight!(A: 0);
tuple_weight!(A: 0, B: 1);
tuple_weight!(A: 0, B: 1, C: 2);
tuple_weight!(A: 0, B: 1, C: 2, D: 3);
tuple_weight!(A: 0, B: 1, C: 2, D: 3, E: 4);
tuple_weight!(A: 0, B: 1, C: 2, D: 3, E: 4, F: 5);

impl CacheWeight for crate::models_diesel::Symbol {
    fn heap_bytes(&self) -> usize {
        self.name.capacity() + self.symbol_path.capacity() + self.leaf_name.capacity()
    }
}
impl CacheWeight for crate::models_diesel::SymbolInstance {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl CacheWeight for crate::models_diesel::Object {
    fn heap_bytes(&self) -> usize {
        self.module_path.capacity()
            + self.filesystem_path.capacity()
            + self.filetype.capacity()
            + self.content_hash.capacity()
    }
}
impl CacheWeight for crate::models_diesel::Project {
    fn heap_bytes(&self) -> usize {
        self.project_name.capacity() + self.root_path.capacity()
    }
}
impl CacheWeight for crate::models_diesel::SymbolRef {
    fn heap_bytes(&self) -> usize {
        0
    }
}
impl CacheWeight for crate::db_diesel::index_impl::ImplicitEdge {
    fn heap_bytes(&self) -> usize {
        0
    }
}

/// Estimated total bytes of a cached result vector: the Vec's inline buffer
/// plus each row's extra heap, plus a flat per-entry overhead for the key,
/// entry struct, and LRU bookkeeping.
pub fn vec_weight<T: CacheWeight>(v: &Vec<T>) -> usize {
    const ENTRY_OVERHEAD: usize = 256;
    std::mem::size_of::<Vec<T>>() + v.heap_bytes() + ENTRY_OVERHEAD
}

// ============================================================================
// Row identity (for partitioned-branch merge dedup)
// ============================================================================

/// Identity of one result row, used by `cached_load_partitioned` to dedup the
/// persistent ∪ ephemeral branch merge.  Dedup is mandatory, not defensive:
/// the node consumers push one output element per row, so a duplicated row
/// (e.g. an eph branch built without its disjointness guard) would otherwise
/// corrupt results rather than just waste cache bytes.
pub trait RowKey {
    type Key: Eq + std::hash::Hash;
    fn row_key(&self) -> Self::Key;
}

use crate::models_diesel::{Object, Project, Symbol, SymbolInstance, SymbolRef};

/// select_current rows: the instance determines symbol/object/project.
impl RowKey for (Symbol, SymbolInstance, Object, Project) {
    type Key = i64;
    fn row_key(&self) -> i64 {
        self.1.id
    }
}

/// select_parents rows: (ref, symbol, declaration instance, parent instance).
impl RowKey for (SymbolRef, Symbol, SymbolInstance, SymbolInstance) {
    type Key = (i64, i64, i64);
    fn row_key(&self) -> Self::Key {
        (self.0.id, self.2.id, self.3.id)
    }
}

/// select_children rows.
impl RowKey
    for (
        Symbol,
        Symbol,
        SymbolInstance,
        SymbolInstance,
        SymbolRef,
        Object,
    )
{
    type Key = (i64, i64, i64);
    fn row_key(&self) -> Self::Key {
        (self.4.id, self.2.id, self.3.id)
    }
}

/// select_has_parents rows: (child symbol, child instance, parent symbol,
/// parent instance).
impl RowKey for (Symbol, SymbolInstance, Symbol, SymbolInstance) {
    type Key = (i64, i64);
    fn row_key(&self) -> Self::Key {
        (self.1.id, self.3.id)
    }
}

/// select_has_children rows.
impl RowKey for (Symbol, SymbolInstance, Symbol, SymbolInstance, Object) {
    type Key = (i64, i64);
    fn row_key(&self) -> Self::Key {
        (self.1.id, self.3.id)
    }
}

/// find_edges_between rows.
impl RowKey for crate::db_diesel::index_impl::ImplicitEdge {
    type Key = (i64, i64, i64);
    fn row_key(&self) -> Self::Key {
        (self.ref_id, self.from_instance_id, self.to_instance_id)
    }
}

// ============================================================================
// The cache
// ============================================================================

struct Entry {
    value: Arc<dyn Any + Send + Sync>,
    bytes: usize,
}

struct Inner {
    map: lru::LruCache<CacheKey, Entry>,
    used_bytes: usize,
    epoch: u64,
}

/// Byte-budgeted LRU cache of typed SQL results.
///
/// Lock discipline: the mutex is only ever taken inside the non-async
/// methods below, so it is structurally impossible to hold it across an
/// `.await`.
pub struct SqlResultCache {
    inner: Mutex<Inner>,
    max_bytes: usize,
    hits: AtomicU64,
    misses: AtomicU64,
    evictions: AtomicU64,
    oversize_skips: AtomicU64,
}

/// Cancellation-safety guard for mutation paths: arm one immediately
/// before awaiting a mutating transaction; on drop — normal completion OR
/// the handler future being cancelled (client disconnect) between the
/// database COMMIT and the invalidation code — the cache is cleared, so a
/// dropped future can no longer leave stale entries behind indefinitely.
/// Clearing on a rolled-back mutation is harmless (lost warmth only).
///
/// Residual micro-window, documented: a cancellation racing the
/// server-side commit application can fire the clear before the commit
/// becomes visible; a concurrent load in that instant may cache
/// pre-mutation data with a post-clear epoch.  This window is the commit
/// round-trip, versus the previous unbounded gap.
pub struct ClearOnDrop(pub Arc<SqlResultCache>);

impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        self.0.clear();
    }
}

/// Point-in-time counters, primarily for tests and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub oversize_skips: u64,
    pub used_bytes: usize,
    pub entries: usize,
}

impl SqlResultCache {
    /// Lock the inner state, recovering from mutex poisoning: a panic while
    /// a previous holder was mid-update may have left the accounting
    /// inconsistent, so recovery resets the cache to a trivially valid
    /// state (empty, epoch bumped) instead of propagating panics into
    /// every subsequent query and mutation on the server.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                guard.epoch += 1;
                guard.map.clear();
                guard.used_bytes = 0;
                self.inner.clear_poison();
                tracing::warn!("sql result cache mutex was poisoned; cache reset");
                guard
            }
        }
    }

    /// `max_bytes == 0` disables the cache: `get` always misses and
    /// `put_if_epoch` is a no-op, so callers keep a single execution path.
    pub fn new(max_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                map: lru::LruCache::unbounded(),
                used_bytes: 0,
                epoch: 0,
            }),
            max_bytes,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            oversize_skips: AtomicU64::new(0),
        })
    }

    pub fn is_enabled(&self) -> bool {
        self.max_bytes > 0
    }

    /// Epoch snapshot; take BEFORE the DB read that will feed a `put`.
    pub fn epoch(&self) -> u64 {
        self.lock().epoch
    }

    pub fn get<T: Send + Sync + 'static>(&self, key: &CacheKey) -> Option<Arc<Vec<T>>> {
        if !self.is_enabled() {
            return None;
        }
        let mut inner = self.lock();
        match inner.map.get(key) {
            Some(entry) => {
                let value = entry.value.clone();
                drop(inner);
                match value.downcast::<Vec<T>>() {
                    Ok(v) => {
                        self.hits.fetch_add(1, Ordering::Relaxed);
                        tracing::debug!(key = ?&key.sql_hash[..6], "sql cache hit");
                        Some(v)
                    }
                    // Unreachable while TypeId is part of the key; fail
                    // closed as a miss rather than panicking.
                    Err(_) => {
                        self.misses.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                }
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Insert unless the cache was cleared since `epoch` was snapshotted
    /// (the value may then predate an index mutation) or the entry alone
    /// exceeds the whole budget.  Evicts LRU entries until under budget.
    pub fn put_if_epoch<T: Send + Sync + 'static>(
        &self,
        key: CacheKey,
        value: Arc<Vec<T>>,
        bytes: usize,
        epoch: u64,
    ) {
        if !self.is_enabled() {
            return;
        }
        let mut inner = self.lock();
        if inner.epoch != epoch {
            tracing::debug!("sql cache put rejected: epoch advanced (cache cleared mid-load)");
            return;
        }
        if bytes > self.max_bytes {
            self.oversize_skips.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(
                bytes,
                budget = self.max_bytes,
                "sql cache entry oversized; skipped"
            );
            return;
        }
        if let Some(old) = inner.map.put(
            key,
            Entry {
                value: value as Arc<dyn Any + Send + Sync>,
                bytes,
            },
        ) {
            inner.used_bytes -= old.bytes;
        }
        inner.used_bytes += bytes;
        while inner.used_bytes > self.max_bytes {
            match inner.map.pop_lru() {
                Some((_, evicted)) => {
                    inner.used_bytes -= evicted.bytes;
                    self.evictions.fetch_add(1, Ordering::Relaxed);
                }
                None => break,
            }
        }
    }

    /// Drop everything and advance the epoch so in-flight loads that read
    /// pre-clear data cannot insert their results afterwards.
    pub fn clear(&self) {
        let epoch = {
            let mut inner = self.lock();
            inner.epoch += 1;
            inner.map.clear();
            inner.used_bytes = 0;
            inner.epoch
        };
        tracing::info!(epoch, "sql result cache cleared");
    }

    pub fn stats(&self) -> CacheStats {
        let inner = self.lock();
        CacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
            oversize_skips: self.oversize_skips.load(Ordering::Relaxed),
            used_bytes: inner.used_bytes,
            entries: inner.map.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: usize) -> Arc<Vec<String>> {
        // One String with capacity n so weights are predictable-ish; we pass
        // explicit byte sizes to put_if_epoch in these tests anyway.
        Arc::new(vec![String::with_capacity(n)])
    }

    #[test]
    fn byte_eviction_is_lru_ordered() {
        let cache = SqlResultCache::new(1000);
        let (k1, k2, k3) = (
            CacheKey::for_test::<String>(b"k1"),
            CacheKey::for_test::<String>(b"k2"),
            CacheKey::for_test::<String>(b"k3"),
        );
        let epoch = cache.epoch();
        cache.put_if_epoch(k1.clone(), entry(1), 400, epoch);
        cache.put_if_epoch(k2.clone(), entry(1), 400, epoch);
        // Touch k1 so k2 becomes LRU.
        assert!(cache.get::<String>(&k1).is_some());
        // Inserting 400 more forces one eviction: k2 must go, k1 stays.
        cache.put_if_epoch(k3.clone(), entry(1), 400, epoch);
        assert!(cache.get::<String>(&k1).is_some(), "recently used survives");
        assert!(cache.get::<String>(&k2).is_none(), "LRU entry evicted");
        assert!(cache.get::<String>(&k3).is_some());
        assert_eq!(cache.stats().evictions, 1);
        assert!(cache.stats().used_bytes <= 1000);
    }

    #[test]
    fn oversized_entry_skipped() {
        let cache = SqlResultCache::new(100);
        let k = CacheKey::for_test::<String>(b"big");
        cache.put_if_epoch(k.clone(), entry(1), 101, cache.epoch());
        assert!(cache.get::<String>(&k).is_none());
        assert_eq!(cache.stats().oversize_skips, 1);
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn stale_epoch_put_rejected() {
        let cache = SqlResultCache::new(1000);
        let k = CacheKey::for_test::<String>(b"stale");
        let epoch = cache.epoch();
        cache.clear(); // simulates an index mutation committing mid-load
        cache.put_if_epoch(k.clone(), entry(1), 10, epoch);
        assert!(
            cache.get::<String>(&k).is_none(),
            "pre-clear read must not be cached after the clear"
        );
    }

    #[test]
    fn clear_empties_and_bumps_epoch() {
        let cache = SqlResultCache::new(1000);
        let k = CacheKey::for_test::<String>(b"c");
        let e0 = cache.epoch();
        cache.put_if_epoch(k.clone(), entry(1), 10, e0);
        assert!(cache.get::<String>(&k).is_some());
        cache.clear();
        assert!(cache.get::<String>(&k).is_none());
        assert_eq!(cache.stats().used_bytes, 0);
        assert!(cache.epoch() > e0);
    }

    #[test]
    fn typed_downcast_roundtrip_and_type_separation() {
        let cache = SqlResultCache::new(1000);
        let ks = CacheKey::for_test::<String>(b"same-tag");
        let ki = CacheKey::for_test::<i64>(b"same-tag");
        assert_ne!(ks, ki, "TypeId separates identical SQL hashes");
        let epoch = cache.epoch();
        cache.put_if_epoch(ks.clone(), Arc::new(vec!["x".to_string()]), 10, epoch);
        cache.put_if_epoch(ki.clone(), Arc::new(vec![7i64]), 10, epoch);
        assert_eq!(cache.get::<String>(&ks).unwrap()[0], "x");
        assert_eq!(cache.get::<i64>(&ki).unwrap()[0], 7);
    }

    #[test]
    fn disabled_cache_is_pass_through() {
        let cache = SqlResultCache::new(0);
        let k = CacheKey::for_test::<String>(b"off");
        cache.put_if_epoch(k.clone(), entry(1), 10, cache.epoch());
        assert!(cache.get::<String>(&k).is_none());
        assert_eq!(cache.stats().entries, 0);
    }

    #[test]
    fn clear_on_drop_clears_even_without_explicit_call() {
        let cache = SqlResultCache::new(1000);
        let k = CacheKey::for_test::<String>(b"guarded");
        cache.put_if_epoch(k.clone(), entry(1), 10, cache.epoch());
        assert!(cache.get::<String>(&k).is_some());
        {
            let _guard = ClearOnDrop(cache.clone());
            // Simulates the mutation future being dropped (cancelled)
            // before any explicit post-commit clear runs.
        }
        assert!(
            cache.get::<String>(&k).is_none(),
            "guard drop must clear the cache"
        );
    }

    #[test]
    fn poisoned_mutex_recovers_with_reset() {
        let cache = SqlResultCache::new(1000);
        let k = CacheKey::for_test::<String>(b"poison");
        cache.put_if_epoch(k.clone(), entry(1), 10, cache.epoch());
        let e0 = cache.epoch();

        // Poison the mutex: panic while holding the guard on another thread.
        let cache2 = cache.clone();
        let _ = std::thread::spawn(move || {
            let _guard = cache2.inner.lock().unwrap();
            panic!("poison the cache mutex");
        })
        .join();

        // Every entry point must recover (reset, not panic).
        assert!(cache.get::<String>(&k).is_none(), "reset drops entries");
        assert!(cache.epoch() > e0, "reset bumps the epoch");
        cache.put_if_epoch(k.clone(), entry(1), 10, cache.epoch());
        assert!(cache.get::<String>(&k).is_some(), "cache usable again");
    }

    #[test]
    fn weight_accounts_string_capacity() {
        let rows = vec![(String::with_capacity(100), 1i64)];
        let w = vec_weight(&rows);
        assert!(w >= 100, "string capacity must be counted, got {}", w);
    }
}
