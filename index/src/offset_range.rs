use std::ops::Bound;

/// Normalise a Postgres `int4range` bound pair to half-open `[start, end)`
/// offsets.  `None` for an unbounded side — callers must decide how to degrade
/// (e.g. scope-fusion falls back to unscoped rather than guessing a range).
/// Saturating: a bound at `i32::MAX` clamps instead of overflowing.
pub fn range_bounds_to_offsets(range: &(Bound<i32>, Bound<i32>)) -> Option<(i32, i32)> {
    let start = match range.0 {
        Bound::Included(value) => value,
        Bound::Excluded(value) => value.saturating_add(1),
        Bound::Unbounded => return None,
    };

    let end = match range.1 {
        Bound::Excluded(value) => value,
        Bound::Included(value) => value.saturating_add(1),
        Bound::Unbounded => return None,
    };

    Some((start, end))
}
