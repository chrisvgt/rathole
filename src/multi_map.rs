use std::collections::HashMap;
use std::hash::Hash;

/// MultiMap is a hash map that can index an item by two keys
/// For example, after an item with key (a, b) is insert, `map.get1(a)` and
/// `map.get2(b)` both returns the item. Likewise the `remove1` and `remove2`.
pub struct MultiMap<K1, K2, V> {
    map1: HashMap<K1, (K2, V)>,
    map2: HashMap<K2, K1>,
}

impl<K1, K2, V> MultiMap<K1, K2, V> {
    pub fn new() -> Self {
        MultiMap {
            map1: HashMap::new(),
            map2: HashMap::new(),
        }
    }
}

#[allow(dead_code)]
impl<K1, K2, V> MultiMap<K1, K2, V>
where
    K1: Hash + Eq + Clone,
    K2: Hash + Eq + Clone,
{
    pub fn insert(&mut self, k1: K1, k2: K2, v: V) -> Result<(), (K1, K2, V)> {
        if self.map1.contains_key(&k1) || self.map2.contains_key(&k2) {
            return Err((k1, k2, v));
        }
        self.map2.insert(k2.clone(), k1.clone());
        self.map1.insert(k1, (k2, v));
        Ok(())
    }

    pub fn get1(&self, k1: &K1) -> Option<&V> {
        self.map1.get(k1).map(|(_, v)| v)
    }

    pub fn get1_mut(&mut self, k1: &K1) -> Option<&mut V> {
        self.map1.get_mut(k1).map(|(_, v)| v)
    }

    pub fn get2(&self, k2: &K2) -> Option<&V> {
        let k1 = self.map2.get(k2)?;
        self.get1(k1)
    }

    pub fn get_mut2(&mut self, k2: &K2) -> Option<&mut V> {
        let k1 = self.map2.get(k2)?;
        self.map1.get_mut(k1).map(|(_, v)| v)
    }

    pub fn remove1(&mut self, k1: &K1) -> Option<V> {
        let (k2, v) = self.map1.remove(k1)?;
        self.map2.remove(&k2);
        Some(v)
    }

    pub fn remove2(&mut self, k2: &K2) -> Option<V> {
        let k1 = self.map2.remove(k2)?;
        self.map1.remove(&k1).map(|(_, v)| v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multi_map() {
        let mut m = MultiMap::new();
        assert!(m.insert(1, "a", 10).is_ok());
        assert!(m.insert(2, "b", 20).is_ok());

        // Either key already in use
        assert_eq!(m.insert(1, "c", 30), Err((1, "c", 30)));
        assert_eq!(m.insert(3, "a", 30), Err((3, "a", 30)));

        assert_eq!(m.get1(&1), Some(&10));
        assert_eq!(m.get2(&"b"), Some(&20));

        if let Some(v) = m.get_mut2(&"a") {
            *v = 11;
        }
        assert_eq!(m.get1(&1), Some(&11));

        // Removing by one key removes the other one too
        assert_eq!(m.remove1(&1), Some(11));
        assert_eq!(m.get2(&"a"), None);
        assert_eq!(m.remove2(&"b"), Some(20));
        assert_eq!(m.get1(&2), None);

        // Both keys can be reused after removal
        assert!(m.insert(1, "a", 12).is_ok());
        assert_eq!(m.get2(&"a"), Some(&12));
    }
}
