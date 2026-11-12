//! Hash maps with a fixed hasher: std's RandomState made packing and
//! placement differ from run to run with the same seed (README (z4)).
pub type HashMap<K, V> = rustc_hash::FxHashMap<K, V>;
pub type HashSet<K> = rustc_hash::FxHashSet<K>;
