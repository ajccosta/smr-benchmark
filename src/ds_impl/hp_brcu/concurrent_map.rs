use hp_brcu::Thread;

pub trait OutputHolder<V> {
    fn default(thread: &mut Thread) -> Self;
    fn output(&self) -> &V;
}

pub trait ConcurrentMap<K, V> {
    type Output: OutputHolder<V>;

    fn empty_output(thread: &mut Thread) -> Self::Output {
        <Self::Output as OutputHolder<V>>::default(thread)
    }

    fn new() -> Self;
    fn get(&self, key: &K, output: &mut Self::Output, thread: &mut Thread) -> bool;
    fn insert(&self, key: K, value: V, output: &mut Self::Output, thread: &mut Thread) -> bool;
    fn remove<'domain, 'hp>(&self, key: &K, output: &mut Self::Output, thread: &mut Thread)
        -> bool;
}

#[cfg(test)]
pub mod tests {
    extern crate rand;
    use super::{ConcurrentMap, OutputHolder};
    use crossbeam_utils::thread;
    use hp_brcu::THREAD;
    use rand::prelude::*;
    use std::fmt::Debug;

    const THREADS: i32 = 30;
    const ELEMENTS_PER_THREADS: i32 = 1000;

    pub fn smoke<V, M, F>(to_value: &F)
    where
        V: Eq + Debug,
        M: ConcurrentMap<i32, V> + Send + Sync,
        F: Sync + Fn(&i32) -> V,
    {
        let map = &M::new();

        thread::scope(|s| {
            for t in 0..THREADS {
                s.spawn(move |_| {
                    THREAD.with(|thread| {
                        let thread = &mut **thread.borrow_mut();
                        let output = &mut M::empty_output(thread);
                        let mut rng = rand::thread_rng();
                        let mut keys: Vec<i32> =
                            (0..ELEMENTS_PER_THREADS).map(|k| k * THREADS + t).collect();
                        keys.shuffle(&mut rng);
                        for i in keys {
                            assert!(map.insert(i, to_value(&i), output, thread));
                        }
                    });
                });
            }
        })
        .unwrap();

        thread::scope(|s| {
            for t in 0..(THREADS / 2) {
                s.spawn(move |_| {
                    THREAD.with(|thread| {
                        let thread = &mut **thread.borrow_mut();
                        let output = &mut M::empty_output(thread);
                        let mut rng = rand::thread_rng();
                        let mut keys: Vec<i32> =
                            (0..ELEMENTS_PER_THREADS).map(|k| k * THREADS + t).collect();
                        keys.shuffle(&mut rng);
                        for i in keys {
                            assert!(map.remove(&i, output, thread));
                        }
                    });
                });
            }
        })
        .unwrap();

        thread::scope(|s| {
            for t in (THREADS / 2)..THREADS {
                s.spawn(move |_| {
                    THREAD.with(|thread| {
                        let thread = &mut **thread.borrow_mut();
                        let output = &mut M::empty_output(thread);
                        let mut rng = rand::thread_rng();
                        let mut keys: Vec<i32> =
                            (0..ELEMENTS_PER_THREADS).map(|k| k * THREADS + t).collect();
                        keys.shuffle(&mut rng);
                        for i in keys {
                            assert!(map.get(&i, output, thread));
                            assert_eq!(to_value(&i), *output.output());
                        }
                    });
                });
            }
        })
        .unwrap();
    }
}
