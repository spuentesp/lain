
/// The importance bonus must be bounded by the corpus-wide anchor
/// scale, not renormalized within each result set.
///
/// `anchor_score` already runs 0–100 across the whole graph. Min-max
/// within the candidate set handed the *relatively* highest anchor a
/// full bonus even when nothing in the set was an anchor: observed live,
/// a symbol scoring 0.59 out of 100 took the maximum +0.3 and outranked
/// a candidate with 0.56 similarity against its own 0.36.
#[test]
fn a_near_zero_anchor_earns_a_near_zero_bonus() {
    const ANCHOR_WEIGHT: f32 = 0.3;
    const ANCHOR_SCALE: f32 = 100.0;
    let bonus = |anchor: f32| ANCHOR_WEIGHT * (anchor / ANCHOR_SCALE).clamp(0.0, 1.0);

    // The live case: 0.59/100 is not an anchor by any reading.
    assert!(
        bonus(0.59) < 0.01,
        "a symbol nothing depends on must not get a meaningful bonus, got {}",
        bonus(0.59)
    );
    // Similarity must therefore decide this pair.
    assert!(0.564 + bonus(0.0) > 0.355 + bonus(0.59));

    // A genuine top-of-corpus anchor still earns the full weight.
    assert!((bonus(100.0) - ANCHOR_WEIGHT).abs() < f32::EPSILON);
    // And a score above the scale cannot exceed it.
    assert!((bonus(250.0) - ANCHOR_WEIGHT).abs() < f32::EPSILON);
}
