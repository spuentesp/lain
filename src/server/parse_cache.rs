//! Optional process-wide cache of tree-sitter parse trees.
//!
//! Ingestion (`treesitter::extract_all`) and the sensors
//! (`sensors::util::parse_for_lang`) both parse the same file contents. With a
//! budget set, whichever runs second reuses the tree, keyed by grammar name and
//! a hash of the parsed text.
//!
//! Off by default. The budget comes from `parse_cache_mb` in the federation
//! config and is applied by [`configure`] when that config loads. On a 3-repo
//! Saleor federation the cache added ~1.2 GB of RSS with no measurable speedup,
//! which is why it is opt-in. When enabled it is bounded by an estimated byte
//! budget with LRU eviction.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use lru::LruCache;
use tree_sitter::{Language, Tree};

/// Sources above this size are parsed but not cached. One huge file would
/// otherwise evict everything else.
const MAX_CACHEABLE_SOURCE_BYTES: usize = 2 * 1024 * 1024;

/// Rough tree-sitter memory per source byte, used to charge an entry against
/// the budget. This is an estimate, not a measurement, and it is low: real
/// trees cost more, which is why the cache is opt-in.
const TREE_BYTES_PER_SOURCE_BYTE: usize = 10;

type Key = (&'static str, [u8; 32]);

/// Byte-budgeted LRU of parse trees. Each entry is charged its estimated cost.
pub(crate) struct ParseCache {
    entries: LruCache<Key, (Tree, usize)>,
    used: usize,
    budget: usize,
}

impl ParseCache {
    pub(crate) fn with_budget(budget: usize) -> Self {
        Self {
            entries: LruCache::unbounded(),
            used: 0,
            budget,
        }
    }

    fn get(&mut self, key: &Key) -> Option<Tree> {
        self.entries.get(key).map(|(tree, _)| tree.clone())
    }

    fn insert(&mut self, key: Key, tree: Tree, cost: usize) {
        if cost > self.budget {
            return;
        }
        if let Some((_, old_cost)) = self.entries.pop(&key) {
            self.used -= old_cost;
        }
        while self.used + cost > self.budget {
            let Some((_, (_, evicted_cost))) = self.entries.pop_lru() else {
                break;
            };
            self.used -= evicted_cost;
        }
        self.entries.put(key, (tree, cost));
        self.used += cost;
    }
}

/// Process-wide slot. `None` means disabled.
static CACHE: Mutex<Option<ParseCache>> = Mutex::new(None);

/// Budget in bytes; `0` means disabled. Read without the lock so the common
/// disabled path costs one atomic load.
static BUDGET_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Apply the `parse_cache_mb` setting. `0` disables the cache.
///
/// The setting is process-wide: whichever federation config loaded last
/// decides it for every federation in the process. Reloading with the same
/// budget keeps the cached trees; a different budget drops them.
pub fn configure(budget_mb: usize) {
    configure_slot(&BUDGET_BYTES, &CACHE, budget_mb);
}

fn mb_to_bytes(budget_mb: usize) -> usize {
    budget_mb.saturating_mul(1024 * 1024)
}

fn configure_slot(budget_bytes: &AtomicUsize, slot: &Mutex<Option<ParseCache>>, budget_mb: usize) {
    let budget = mb_to_bytes(budget_mb);
    if budget_bytes.swap(budget, Ordering::AcqRel) == budget {
        return;
    }
    *lock(slot) = (budget > 0).then(|| ParseCache::with_budget(budget));
}

fn budget_enabled(budget_bytes: &AtomicUsize) -> bool {
    budget_bytes.load(Ordering::Acquire) > 0
}

fn lock(slot: &Mutex<Option<ParseCache>>) -> MutexGuard<'_, Option<ParseCache>> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Return the tree for `source` under `language`, reusing a cached one when
/// the same text was already parsed. `parse` runs only on a miss; its result is
/// cached unless it is `None`. Parsing happens outside the lock.
pub fn get_or_parse(
    language: &Language,
    source: &str,
    parse: impl FnOnce() -> Option<Tree>,
) -> Option<Tree> {
    if !budget_enabled(&BUDGET_BYTES) {
        return parse();
    }
    lookup_or_parse(&CACHE, language, source, parse)
}

fn lookup_or_parse(
    slot: &Mutex<Option<ParseCache>>,
    language: &Language,
    source: &str,
    parse: impl FnOnce() -> Option<Tree>,
) -> Option<Tree> {
    let Some(name) = language.name() else {
        return parse();
    };
    if source.len() > MAX_CACHEABLE_SOURCE_BYTES {
        return parse();
    }
    let key: Key = (name, *blake3::hash(source.as_bytes()).as_bytes());
    let hit = lock(slot).as_mut().and_then(|cache| cache.get(&key));
    if let Some(tree) = hit {
        return Some(tree);
    }
    let tree = parse()?;
    let cost = source.len() * TREE_BYTES_PER_SOURCE_BYTE;
    if let Some(cache) = lock(slot).as_mut() {
        cache.insert(key, tree.clone(), cost);
    }
    Some(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_language() -> Language {
        tree_sitter_rust::LANGUAGE.into()
    }

    fn parse_rust(src: &str) -> Option<Tree> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&rust_language()).ok()?;
        parser.parse(src, None)
    }

    #[test]
    fn hit_returns_cached_tree_without_parsing() {
        let mut cache = ParseCache::with_budget(1024 * 1024);
        let key: Key = ("rust", *blake3::hash(b"fn a() {}").as_bytes());
        cache.insert(key, parse_rust("fn a() {}").unwrap(), 100);
        assert!(cache.get(&key).is_some());
    }

    #[test]
    fn evicts_least_recently_used_to_stay_within_budget() {
        let mut cache = ParseCache::with_budget(250);
        let key = |s: &[u8]| -> Key { ("rust", *blake3::hash(s).as_bytes()) };
        let tree = parse_rust("fn a() {}").unwrap();
        cache.insert(key(b"a"), tree.clone(), 100);
        cache.insert(key(b"b"), tree.clone(), 100);
        // Touch "a" so "b" becomes the least recently used entry.
        assert!(cache.get(&key(b"a")).is_some());
        cache.insert(key(b"c"), tree.clone(), 100);

        assert!(cache.used <= 250, "used {} over budget", cache.used);
        assert!(
            cache.get(&key(b"b")).is_none(),
            "LRU entry should be evicted"
        );
        assert!(cache.get(&key(b"a")).is_some());
        assert!(cache.get(&key(b"c")).is_some());
    }

    #[test]
    fn entry_larger_than_budget_is_not_cached() {
        let mut cache = ParseCache::with_budget(50);
        let key: Key = ("rust", *blake3::hash(b"big").as_bytes());
        cache.insert(key, parse_rust("fn a() {}").unwrap(), 100);
        assert_eq!(cache.used, 0);
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn lookup_or_parse_skips_parse_on_second_call() {
        let slot = Mutex::new(Some(ParseCache::with_budget(1024 * 1024)));
        let lang = rust_language();
        let src = "fn only_once() {}";
        let mut calls = 0;
        let first = lookup_or_parse(&slot, &lang, src, || {
            calls += 1;
            parse_rust(src)
        });
        let second = lookup_or_parse(&slot, &lang, src, || {
            calls += 1;
            parse_rust(src)
        });
        assert!(first.is_some() && second.is_some());
        assert_eq!(calls, 1);
    }

    #[test]
    fn disabled_slot_always_parses() {
        let slot: Mutex<Option<ParseCache>> = Mutex::new(None);
        let lang = rust_language();
        let src = "fn never_cached() {}";
        let mut calls = 0;
        for _ in 0..2 {
            lookup_or_parse(&slot, &lang, src, || {
                calls += 1;
                parse_rust(src)
            });
        }
        assert_eq!(calls, 2);
    }

    fn sized_source(len: usize) -> String {
        let unit = "fn a() {}\n";
        let mut source = unit.repeat(len / unit.len() + 1);
        source.truncate(len);
        source
    }

    #[test]
    fn insert_accepts_an_entry_exactly_at_the_budget() {
        let mut cache = ParseCache::with_budget(100);
        cache.insert(("rust", [1; 32]), parse_rust("fn a() {}").unwrap(), 100);
        assert_eq!(cache.used, 100);
        assert!(cache.get(&("rust", [1; 32])).is_some());
    }

    #[test]
    fn replacing_an_entry_charges_only_its_new_cost() {
        let mut cache = ParseCache::with_budget(1000);
        let key = ("rust", [2; 32]);
        cache.insert(key, parse_rust("fn a() {}").unwrap(), 100);
        cache.insert(key, parse_rust("fn a() {}").unwrap(), 30);
        assert_eq!(cache.used, 30);
    }

    #[test]
    fn eviction_keeps_the_byte_count_exact() {
        let mut cache = ParseCache::with_budget(250);
        let tree = parse_rust("fn a() {}").unwrap();
        cache.insert(("rust", [3; 32]), tree.clone(), 100);
        cache.insert(("rust", [4; 32]), tree.clone(), 100);
        cache.insert(("rust", [5; 32]), tree.clone(), 100);
        assert_eq!(cache.used, 200);
        assert_eq!(cache.entries.len(), 2);
    }

    #[test]
    fn an_entry_that_exactly_fills_the_budget_evicts_nothing() {
        let mut cache = ParseCache::with_budget(200);
        let tree = parse_rust("fn a() {}").unwrap();
        cache.insert(("rust", [6; 32]), tree.clone(), 100);
        cache.insert(("rust", [7; 32]), tree.clone(), 100);
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.used, 200);
    }

    #[test]
    fn budget_is_megabytes() {
        assert_eq!(mb_to_bytes(1), 1024 * 1024);
        assert_eq!(mb_to_bytes(0), 0);
    }

    #[test]
    fn configure_keeps_entries_for_the_same_budget_and_drops_them_otherwise() {
        let budget = AtomicUsize::new(0);
        let slot = Mutex::new(None);
        configure_slot(&budget, &slot, 1);
        assert_eq!(budget.load(Ordering::Acquire), 1024 * 1024);
        lock(&slot).as_mut().unwrap().insert(
            ("rust", [8; 32]),
            parse_rust("fn a() {}").unwrap(),
            10,
        );
        configure_slot(&budget, &slot, 1);
        assert_eq!(
            lock(&slot).as_ref().unwrap().used,
            10,
            "same budget keeps entries"
        );
        configure_slot(&budget, &slot, 2);
        assert_eq!(
            lock(&slot).as_ref().unwrap().used,
            0,
            "new budget starts empty"
        );
        configure_slot(&budget, &slot, 0);
        assert!(lock(&slot).is_none(), "zero disables the cache");
        assert!(!budget_enabled(&budget));
    }

    #[test]
    fn budget_enabled_only_when_positive() {
        assert!(!budget_enabled(&AtomicUsize::new(0)));
        assert!(budget_enabled(&AtomicUsize::new(1)));
    }

    #[test]
    fn sources_up_to_the_size_limit_are_cached_and_larger_ones_are_not() {
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        let limit = MAX_CACHEABLE_SOURCE_BYTES;
        for (len, cached) in [(1_500_000usize, true), (limit, true), (limit + 1, false)] {
            let slot = Mutex::new(Some(ParseCache::with_budget(usize::MAX / 2)));
            let src = sized_source(len);
            let mut calls = 0;
            for _ in 0..2 {
                lookup_or_parse(&slot, &lang, &src, || {
                    calls += 1;
                    parse_rust(&src)
                });
            }
            let expected = if cached { 1 } else { 2 };
            assert_eq!(calls, expected, "source of {len} bytes");
        }
    }

    #[test]
    fn a_cached_entry_is_charged_ten_bytes_per_source_byte() {
        let lang: Language = tree_sitter_rust::LANGUAGE.into();
        let slot = Mutex::new(Some(ParseCache::with_budget(1000)));
        let src = "fn a() {}";
        lookup_or_parse(&slot, &lang, src, || parse_rust(src));
        assert_eq!(
            lock(&slot).as_ref().unwrap().used,
            src.len() * TREE_BYTES_PER_SOURCE_BYTE
        );
    }
}
