//! Target binding reuse adapted from fork-cgbj f4b48ad7 (GPL-3.0-or-later).
#[derive(Default)]
pub(crate) struct BindingCache<K> { last: Option<K> }
impl<K: PartialEq> BindingCache<K> {
    pub(crate) fn update(&mut self, key: K) -> bool {
        if self.last.as_ref() == Some(&key) { return false; }
        self.last = Some(key);
        true
    }
    pub(crate) fn clear(&mut self) { self.last = None; }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bindings_compare_resources_descriptions_order_and_depth() {
        let mut c = BindingCache::default();
        let a = (vec![(7usize, [1u32; 7]), (0, [2;7])], Some((9, [3;7])));
        assert!(c.update(a.clone()));
        assert!(!c.update(a.clone()));
        let mut changed=a.clone(); changed.0[0].1[2]=4;
        assert!(c.update(changed));
        assert!(c.update(a.clone()));
        let mut moved=a.clone(); moved.0[0].0=8;
        assert!(c.update(moved));
        let mut ordered=a.clone(); ordered.0.reverse();
        assert!(c.update(ordered));
        assert!(c.update((a.0.clone(), None)));
        assert!(c.update((Vec::new(), None)));
        assert!(!c.update((Vec::new(), None)));
        c.clear();
        assert!(c.update((Vec::new(), None)));
    }
}
