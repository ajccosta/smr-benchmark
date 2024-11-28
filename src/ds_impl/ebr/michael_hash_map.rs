use super::concurrent_map::{ConcurrentMap, OutputHolder};
use crossbeam_ebr::Guard;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use super::list::HHSList;

pub struct HashMap<K, V> {
    buckets: Vec<HHSList<K, V>>,
}

impl<K, V> HashMap<K, V>
where
    K: Ord + Hash + Default,
    V: Default,
{
    pub fn with_capacity(n: usize) -> Self {
        let mut buckets = Vec::with_capacity(n);
        for _ in 0..n {
            buckets.push(HHSList::new());
        }

        HashMap { buckets }
    }

    #[inline]
    pub fn get_bucket(&self, index: usize) -> &HHSList<K, V> {
        unsafe { self.buckets.get_unchecked(index % self.buckets.len()) }
    }

    #[inline]
    fn hash(k: &K) -> usize {
        let mut s = DefaultHasher::new();
        k.hash(&mut s);
        s.finish() as usize
    }

    pub fn get<'g>(&'g self, k: &'g K, guard: &'g Guard) -> Option<impl OutputHolder<V> + 'g> {
        let i = Self::hash(k);
        self.get_bucket(i).get(k, guard)
    }

    pub fn insert(&self, k: K, v: V, guard: &Guard) -> bool {
        let i = Self::hash(&k);
        self.get_bucket(i).insert(k, v, guard)
    }

    pub fn remove<'g>(&'g self, k: &'g K, guard: &'g Guard) -> Option<impl OutputHolder<V> + 'g> {
        let i = Self::hash(k);
        self.get_bucket(i).remove(k, guard)
    }
}

impl<K, V> ConcurrentMap<K, V> for HashMap<K, V>
where
    K: Ord + Hash + Default,
    V: Default,
{
    fn new() -> Self {
        Self::with_capacity(30000)
    }

    #[inline(always)]
    fn get<'g>(&'g self, key: &'g K, guard: &'g Guard) -> Option<impl OutputHolder<V>> {
        self.get(key, guard)
    }
    #[inline(always)]
    fn insert(&self, key: K, value: V, guard: &Guard) -> bool {
        self.insert(key, value, guard)
    }
    #[inline(always)]
    fn remove<'g>(&'g self, key: &'g K, guard: &'g Guard) -> Option<impl OutputHolder<V>> {
        self.remove(key, guard)
    }
}

#[cfg(test)]
mod tests {
    use super::HashMap;
    use crate::ds_impl::ebr::concurrent_map;

    #[test]
    fn smoke_hashmap() {
        concurrent_map::tests::smoke::<_, HashMap<i32, String>, _>(&i32::to_string);
    }
}
