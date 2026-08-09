/// The normaliser lives in the index crate so DB-adjacent code (scope-fusion
/// range resolution) can share it; re-exported here for existing callers.
pub use index::offset_range::range_bounds_to_offsets;
