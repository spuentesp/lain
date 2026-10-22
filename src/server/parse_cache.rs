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

/// Apply the `parse_cache_mb` setting. `0` disables the cache and drops any
/// entries. Called when the federation config loads.
pub fn configure(budget_mb: usize) {
    let budget = budget_mb.saturating_mul(1024 * 1024);
    BUDGET_BYTES.store(budget, Ordering::Release);
    *lock(&CACHE) = (budget > 0).then(|| ParseCache::with_budget(budget));
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
    if BUDGET_BYTES.load(Ordering::Acquire) == 0 {
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
}
