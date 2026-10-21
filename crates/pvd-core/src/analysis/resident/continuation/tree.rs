use super::*;
use containers::{Item, TreeKey};
use owner::InlineCow;
use serde::ser::{SerializeMap, SerializeSeq};

const MAX_ENTRIES: usize = 1_000_000;
const MAX_HEIGHT: usize = MAX_AVL_HEIGHT as usize;
type Link<K, V> = Option<Owned<Node<K, V>>>;
pub(super) mod pages;
#[cfg(test)]
mod tests;

#[derive(Clone)]
struct Node<K, V> {
    key: K,
    value: V,
    left: Link<K, V>,
    right: Link<K, V>,
    height: u8,
    len: usize,
}
impl<K: TreeKey, V: Clone + Item> InlineCow for Node<K, V> {
    fn copied(domain: &Domain) {
        owner::cumulative(&domain.0.tree_copied_entries, 1);
        owner::cumulative(
            &domain.0.tree_copied_layout_bytes,
            size_of::<K>() + size_of::<V>(),
        );
    }
}

pub(super) struct Tree<K, V> {
    root: Link<K, V>,
    len: usize,
    domain: Domain,
}
impl<K, V> Clone for Tree<K, V> {
    fn clone(&self) -> Self {
        Self {
            root: self.root.clone(),
            len: self.len,
            domain: self.domain.clone(),
        }
    }
}
impl<K, V> Item for Tree<K, V> {
    fn same_domain(&self, domain: &Domain) -> bool {
        self.domain.same(domain)
    }
}
impl<K: TreeKey, V> Tree<K, V> {
    pub(super) fn empty(domain: &Domain) -> Result<Self, Error> {
        Ok(Self {
            root: None,
            len: 0,
            domain: domain.clone(),
        })
    }
    pub(super) fn domain(&self) -> &Domain {
        &self.domain
    }
    pub(super) fn len(&self) -> usize {
        self.len
    }
    pub(super) fn get(&self, key: &K) -> Option<&V> {
        let mut next = self.root.as_deref();
        while let Some(node) = next {
            match key.cmp(&node.key) {
                std::cmp::Ordering::Less => next = node.left.as_deref(),
                std::cmp::Ordering::Greater => next = node.right.as_deref(),
                std::cmp::Ordering::Equal => return Some(&node.value),
            }
        }
        None
    }
    pub(super) fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }
    #[cfg(test)]
    pub(super) fn same(&self, other: &Self) -> bool {
        self.domain.same(&other.domain)
            && match (&self.root, &other.root) {
                (None, None) => true,
                (Some(left), Some(right)) => left.same(right),
                _ => false,
            }
    }
    pub(super) fn iter(&self) -> Iter<'_, K, V> {
        Iter::new(self)
    }
    pub(super) fn values(&self) -> impl DoubleEndedIterator<Item = &V> + ExactSizeIterator {
        self.iter().map(|(_, value)| value)
    }
}
impl<K: TreeKey, V: Clone + Item> Tree<K, V> {
    pub(super) fn edit(
        &self,
        domain: &Domain,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<TreeDraft<K, V>, Error> {
        check_replay_cancelled(cancelled)?;
        if !self.domain.same(domain) {
            return Err(error("resident tree belongs to another domain"));
        }
        Ok(TreeDraft {
            tree: self.clone(),
            failed: false,
        })
    }
    fn insert(
        &mut self,
        key: K,
        value: V,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Option<V>, Error> {
        check_replay_cancelled(cancelled)?;
        if !value.same_domain(&self.domain) {
            return Err(error("resident tree cannot adopt a foreign value"));
        }
        if self.len >= MAX_ENTRIES && !self.contains_key(&key) {
            return Err(error("resident tree exceeds its entry limit"));
        }
        let mut draft = self.root.clone();
        let replaced = Node::insert(&mut draft, key, value, &self.domain, cancelled)?;
        let len = add(self.len, usize::from(replaced.is_none()))?;
        check_replay_cancelled(cancelled)?;
        self.root = draft;
        self.len = len;
        Ok(replaced)
    }
    fn remove(&mut self, key: &K, cancelled: &mut dyn FnMut() -> bool) -> Result<Option<V>, Error> {
        check_replay_cancelled(cancelled)?;
        if !self.contains_key(key) {
            return Ok(None);
        }
        let mut draft = self.root.clone();
        let removed = Node::remove(&mut draft, key, &self.domain, cancelled)?;
        check_replay_cancelled(cancelled)?;
        self.root = draft;
        self.len -= usize::from(removed.is_some());
        Ok(removed)
    }
    #[cfg(test)]
    fn validate(&self) -> Result<(), Error> {
        fn node<K: TreeKey, V: Clone + Item>(
            link: &Link<K, V>,
            lower: Option<K>,
            upper: Option<K>,
            domain: &Domain,
        ) -> Result<(usize, u8), Error> {
            let Some(root) = link else {
                return Ok((0, 0));
            };
            if !root.domain().same(domain)
                || !root.value.same_domain(domain)
                || lower.is_some_and(|key| root.key <= key)
                || upper.is_some_and(|key| root.key >= key)
            {
                return Err(error("resident tree ownership or key bounds disagree"));
            }
            let (lc, lh) = node(&root.left, lower, Some(root.key), domain)?;
            let (rc, rh) = node(&root.right, Some(root.key), upper, domain)?;
            let len = add(add(lc, rc)?, 1)?;
            let height = 1 + lh.max(rh);
            if lh.abs_diff(rh) > 1
                || root.height != height
                || usize::from(height) > MAX_HEIGHT
                || root.len != len
            {
                return Err(error("resident tree AVL shape or subtree count disagrees"));
            }
            Ok((len, height))
        }
        if node(&self.root, None, None, &self.domain)?.0 != self.len {
            return Err(error("resident tree length disagrees"));
        }
        Ok(())
    }
}

pub(super) struct TreeDraft<K, V> {
    tree: Tree<K, V>,
    failed: bool,
}
impl<K: TreeKey, V: Clone + Item> TreeDraft<K, V> {
    #[cfg(test)]
    pub(super) fn new(domain: &Domain, hint: usize) -> Result<Self, Error> {
        if hint > MAX_ENTRIES {
            return Err(error("resident tree exceeds its entry limit"));
        }
        Ok(Self {
            tree: Tree::empty(domain)?,
            failed: false,
        })
    }
    pub(super) fn len(&self) -> usize {
        self.tree.len()
    }
    pub(super) fn get(&self, key: &K) -> Option<&V> {
        self.tree.get(key)
    }
    #[cfg(test)]
    pub(super) fn insert(&mut self, key: K, value: V) -> Result<Option<V>, Error> {
        self.insert_with(key, value, &mut || false)
    }
    #[cfg(test)]
    pub(super) fn remove(&mut self, key: &K) -> Result<Option<V>, Error> {
        self.remove_with(key, &mut || false)
    }
    fn valid(&self) -> Result<(), Error> {
        if self.failed {
            Err(error("resident tree draft has failed"))
        } else {
            Ok(())
        }
    }
    fn insert_with(
        &mut self,
        key: K,
        value: V,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Option<V>, Error> {
        self.valid()?;
        let result = self.tree.insert(key, value, cancelled);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn remove_with(
        &mut self,
        key: &K,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Option<V>, Error> {
        self.valid()?;
        let result = self.tree.remove(key, cancelled);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub(super) fn finish(self) -> Result<Tree<K, V>, Error> {
        self.valid()?;
        Ok(self.tree)
    }
}

pub(super) struct TreeEdit<K, V> {
    source: Tree<K, V>,
    draft: Option<TreeDraft<K, V>>,
    failed: bool,
}
impl<K: TreeKey, V: Clone + Item> TreeEdit<K, V> {
    pub(super) fn new(source: &Tree<K, V>) -> Self {
        Self {
            source: source.clone(),
            draft: None,
            failed: false,
        }
    }
    pub(super) fn get(&self, key: &K) -> Option<&V> {
        self.draft
            .as_ref()
            .map_or_else(|| self.source.get(key), |draft| draft.get(key))
    }
    pub(super) fn len(&self) -> usize {
        self.draft
            .as_ref()
            .map_or_else(|| self.source.len(), TreeDraft::len)
    }
    pub(super) fn edit(
        &mut self,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<&mut TreeDraft<K, V>, Error> {
        if self.failed {
            return Err(error("resident tree editor has failed"));
        }
        if let Err(error) = check_replay_cancelled(cancelled) {
            self.failed = true;
            return Err(error);
        }
        if self.draft.is_none() {
            match self.source.edit(self.source.domain(), cancelled) {
                Ok(draft) => self.draft = Some(draft),
                Err(error) => {
                    self.failed = true;
                    return Err(error);
                }
            }
        }
        Ok(self.draft.as_mut().unwrap())
    }
    pub(super) fn insert(
        &mut self,
        key: K,
        value: V,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Option<V>, Error> {
        let result = self.edit(cancelled)?.insert_with(key, value, cancelled);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub(super) fn remove(
        &mut self,
        key: &K,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Option<V>, Error> {
        let result = self.edit(cancelled)?.remove_with(key, cancelled);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub(super) fn finish(self) -> Result<Tree<K, V>, Error> {
        if self.failed {
            return Err(error("resident tree editor has failed"));
        }
        self.draft
            .map_or_else(|| Ok(self.source), TreeDraft::finish)
    }
}

impl<K: TreeKey, V: Clone + Item> Node<K, V> {
    fn height(link: &Link<K, V>) -> u8 {
        link.as_ref().map_or(0, |node| node.height)
    }
    fn count(link: &Link<K, V>) -> usize {
        link.as_ref().map_or(0, |node| node.len)
    }
    fn refresh(&mut self) -> Result<(), Error> {
        self.height = 1 + Self::height(&self.left).max(Self::height(&self.right));
        self.len = add(add(Self::count(&self.left), Self::count(&self.right))?, 1)?;
        Ok(())
    }
    fn balance(&self) -> i16 {
        i16::from(Self::height(&self.left)) - i16::from(Self::height(&self.right))
    }
    fn rotate_left(
        root: &mut Owned<Self>,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut pivot = root
            .make_mut()?
            .right
            .take()
            .ok_or_else(|| error("resident tree rotation lacks pivot"))?;
        root.make_mut()?.right = pivot.make_mut()?.left.take();
        root.make_mut()?.refresh()?;
        pivot.make_mut()?.left = Some(root.clone());
        pivot.make_mut()?.refresh()?;
        *root = pivot;
        Ok(())
    }
    fn rotate_right(
        root: &mut Owned<Self>,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut pivot = root
            .make_mut()?
            .left
            .take()
            .ok_or_else(|| error("resident tree rotation lacks pivot"))?;
        root.make_mut()?.left = pivot.make_mut()?.right.take();
        root.make_mut()?.refresh()?;
        pivot.make_mut()?.right = Some(root.clone());
        pivot.make_mut()?.refresh()?;
        *root = pivot;
        Ok(())
    }
    fn rebalance(root: &mut Owned<Self>, cancelled: &mut dyn FnMut() -> bool) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        root.make_mut()?.refresh()?;
        match root.balance() {
            2.. => {
                if root.left.as_ref().unwrap().balance() < 0 {
                    Self::rotate_left(root.make_mut()?.left.as_mut().unwrap(), cancelled)?;
                }
                Self::rotate_right(root, cancelled)?;
            }
            ..=-2 => {
                if root.right.as_ref().unwrap().balance() > 0 {
                    Self::rotate_right(root.make_mut()?.right.as_mut().unwrap(), cancelled)?;
                }
                Self::rotate_left(root, cancelled)?;
            }
            _ => (),
        }
        Ok(())
    }
    fn insert(
        link: &mut Link<K, V>,
        key: K,
        value: V,
        domain: &Domain,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Option<V>, Error> {
        check_replay_cancelled(cancelled)?;
        let Some(root) = link else {
            *link = Some(Owned::new(
                Self {
                    key,
                    value,
                    left: None,
                    right: None,
                    height: 1,
                    len: 1,
                },
                domain,
                0,
            )?);
            return Ok(None);
        };
        let node = root.make_mut()?;
        let replaced = match key.cmp(&node.key) {
            std::cmp::Ordering::Less => {
                Self::insert(&mut node.left, key, value, domain, cancelled)?
            }
            std::cmp::Ordering::Greater => {
                Self::insert(&mut node.right, key, value, domain, cancelled)?
            }
            std::cmp::Ordering::Equal => {
                return Ok(Some(std::mem::replace(&mut node.value, value)));
            }
        };
        Self::rebalance(root, cancelled)?;
        Ok(replaced)
    }
    fn remove(
        link: &mut Link<K, V>,
        key: &K,
        domain: &Domain,
        cancelled: &mut dyn FnMut() -> bool,
    ) -> Result<Option<V>, Error> {
        check_replay_cancelled(cancelled)?;
        let Some(root) = link.as_mut() else {
            return Ok(None);
        };
        if !root.domain().same(domain) {
            return Err(error("resident tree subtree belongs to another domain"));
        }
        let node = root.make_mut()?;
        let removed = match key.cmp(&node.key) {
            std::cmp::Ordering::Less => Self::remove(&mut node.left, key, domain, cancelled)?,
            std::cmp::Ordering::Greater => Self::remove(&mut node.right, key, domain, cancelled)?,
            std::cmp::Ordering::Equal => {
                let value = node.value.clone();
                if node.left.is_none() {
                    *link = node.right.take();
                    return Ok(Some(value));
                }
                if node.right.is_none() {
                    *link = node.left.take();
                    return Ok(Some(value));
                }
                let mut successor = node.right.as_deref().unwrap();
                while let Some(left) = successor.left.as_deref() {
                    successor = left;
                }
                node.key = successor.key;
                node.value = successor.value.clone();
                Self::remove(&mut node.right, &node.key, domain, cancelled)?;
                Some(value)
            }
        };
        Self::rebalance(root, cancelled)?;
        Ok(removed)
    }
}

pub(super) struct Iter<'a, K, V> {
    front: [Option<&'a Node<K, V>>; MAX_HEIGHT],
    back: [Option<&'a Node<K, V>>; MAX_HEIGHT],
    front_len: usize,
    back_len: usize,
    remaining: usize,
}
impl<'a, K, V> Iter<'a, K, V> {
    fn new(tree: &'a Tree<K, V>) -> Self {
        let mut iter = Self {
            front: [None; MAX_HEIGHT],
            back: [None; MAX_HEIGHT],
            front_len: 0,
            back_len: 0,
            remaining: tree.len,
        };
        iter.left(tree.root.as_deref());
        iter.right(tree.root.as_deref());
        iter
    }
    fn left(&mut self, mut next: Option<&'a Node<K, V>>) {
        while let Some(node) = next {
            self.front[self.front_len] = Some(node);
            self.front_len += 1;
            next = node.left.as_deref();
        }
    }
    fn right(&mut self, mut next: Option<&'a Node<K, V>>) {
        while let Some(node) = next {
            self.back[self.back_len] = Some(node);
            self.back_len += 1;
            next = node.right.as_deref();
        }
    }
}
impl<'a, K, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.front_len -= 1;
        let node = self.front[self.front_len].take().unwrap();
        self.left(node.right.as_deref());
        self.remaining -= 1;
        Some((&node.key, &node.value))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}
impl<K, V> DoubleEndedIterator for Iter<'_, K, V> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.back_len -= 1;
        let node = self.back[self.back_len].take().unwrap();
        self.right(node.left.as_deref());
        self.remaining -= 1;
        Some((&node.key, &node.value))
    }
}
impl<K, V> ExactSizeIterator for Iter<'_, K, V> {}
impl<K, V> std::iter::FusedIterator for Iter<'_, K, V> {}
impl<K: TreeKey + Serialize, V: Serialize> Serialize for Tree<K, V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, value) in self.iter() {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
pub(super) struct Set<K>(pub(super) Tree<K, ()>);
impl<K> Clone for Set<K> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}
impl<K: TreeKey + Serialize> Serialize for Set<K> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for (key, ()) in self.0.iter() {
            sequence.serialize_element(key)?;
        }
        sequence.end()
    }
}
