//! `take_batch` must partition `pending`: nothing is lost, nothing is
//! duplicated, and the batch honours the limit. (It once used
//! `HashSet::drain().take(limit)`, which silently discarded the overflow.)
use super::*;
use proptest::prelude::*;

proptest! {
    #[test]
    fn take_batch_partitions_pending(
        names in prop::collection::hash_set("[a-z]{1,6}", 0..60),
        limit in 0usize..40,
    ) {
        let original: HashSet<PathBuf> = names.iter().map(PathBuf::from).collect();
        let mut pending = original.clone();
        let batch = take_batch(&mut pending, limit);

        prop_assert_eq!(batch.len(), limit.min(original.len()));
        let batch_set: HashSet<PathBuf> = batch.iter().cloned().collect();
        prop_assert_eq!(batch_set.len(), batch.len(), "duplicate in batch");
        prop_assert!(batch_set.is_disjoint(&pending), "taken path still pending");
        let rejoined: HashSet<PathBuf> = batch_set.union(&pending).cloned().collect();
        prop_assert_eq!(rejoined, original, "a path was lost or invented");
    }

    /// Repeated draining terminates and yields every path exactly once.
    #[test]
    fn draining_in_batches_yields_every_path_once(
        names in prop::collection::hash_set("[a-z]{1,6}", 0..80),
        limit in 1usize..25,
    ) {
        let mut pending: HashSet<PathBuf> = names.iter().map(PathBuf::from).collect();
        let total = pending.len();
        let mut seen = HashSet::new();
        let mut rounds = 0;
        while !pending.is_empty() {
            for p in take_batch(&mut pending, limit) {
                prop_assert!(seen.insert(p), "path yielded twice");
            }
            rounds += 1;
            prop_assert!(rounds <= total + 1, "did not terminate");
        }
        prop_assert_eq!(seen.len(), total);
    }
}
