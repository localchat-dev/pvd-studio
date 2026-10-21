use super::Allocation;
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeMap};
use std::{collections::BTreeMap, fmt, iter::FusedIterator, ops::Index, sync::Arc};

type Key = (u64, u64);
type Value = Arc<Allocation>;
type Link = Option<Arc<Node>>;

#[derive(Clone)]
struct Node {
    key: Key,
    value: Value,
    left: Link,
    right: Link,
    height: u8,
    subtree_len: usize,
}

impl Node {
    fn height(link: &Link) -> u8 {
        link.as_ref().map_or(0, |node| node.height)
    }

    fn refresh(&mut self) {
        self.height = 1 + Self::height(&self.left).max(Self::height(&self.right));
        self.subtree_len = 1 + Self::len(&self.left) + Self::len(&self.right);
    }

    fn len(link: &Link) -> usize {
        link.as_ref().map_or(0, |node| node.subtree_len)
    }

    fn balance(&self) -> i16 {
        i16::from(Self::height(&self.left)) - i16::from(Self::height(&self.right))
    }

    fn rotate_left(root: &mut Arc<Self>) {
        let mut pivot = Arc::make_mut(root).right.take().unwrap();
        Arc::make_mut(root).right = Arc::make_mut(&mut pivot).left.take();
        Arc::make_mut(root).refresh();
        Arc::make_mut(&mut pivot).left = Some(root.clone());
        Arc::make_mut(&mut pivot).refresh();
        *root = pivot;
    }

    fn rotate_right(root: &mut Arc<Self>) {
        let mut pivot = Arc::make_mut(root).left.take().unwrap();
        Arc::make_mut(root).left = Arc::make_mut(&mut pivot).right.take();
        Arc::make_mut(root).refresh();
        Arc::make_mut(&mut pivot).right = Some(root.clone());
        Arc::make_mut(&mut pivot).refresh();
        *root = pivot;
    }

    fn rebalance(root: &mut Arc<Self>) {
        Arc::make_mut(root).refresh();
        match root.balance() {
            2.. => {
                if root.left.as_ref().unwrap().balance() < 0 {
                    Self::rotate_left(Arc::make_mut(root).left.as_mut().unwrap());
                }
                Self::rotate_right(root);
            }
            ..=-2 => {
                if root.right.as_ref().unwrap().balance() > 0 {
                    Self::rotate_right(Arc::make_mut(root).right.as_mut().unwrap());
                }
                Self::rotate_left(root);
            }
            _ => (),
        }
    }

    fn insert(link: &mut Link, key: Key, value: Value) -> Option<Value> {
        let Some(root) = link else {
            *link = Some(Arc::new(Self {
                key,
                value,
                left: None,
                right: None,
                height: 1,
                subtree_len: 1,
            }));
            return None;
        };
        let node = Arc::make_mut(root);
        let replaced = match key.cmp(&node.key) {
            std::cmp::Ordering::Less => Self::insert(&mut node.left, key, value),
            std::cmp::Ordering::Greater => Self::insert(&mut node.right, key, value),
            std::cmp::Ordering::Equal => return Some(std::mem::replace(&mut node.value, value)),
        };
        Self::rebalance(root);
        replaced
    }

    fn remove(link: &mut Link, key: &Key) -> Option<Value> {
        let root = link.as_mut()?;
        let node = Arc::make_mut(root);
        let removed = match key.cmp(&node.key) {
            std::cmp::Ordering::Less => Self::remove(&mut node.left, key),
            std::cmp::Ordering::Greater => Self::remove(&mut node.right, key),
            std::cmp::Ordering::Equal => {
                let value = node.value.clone();
                if node.left.is_none() {
                    *link = node.right.take();
                    return Some(value);
                }
                if node.right.is_none() {
                    *link = node.left.take();
                    return Some(value);
                }
                let mut successor = node.right.as_deref().unwrap();
                while let Some(left) = successor.left.as_deref() {
                    successor = left;
                }
                node.key = successor.key;
                node.value = successor.value.clone();
                Self::remove(&mut node.right, &node.key);
                Some(value)
            }
        };
        Self::rebalance(root);
        removed
    }
}

/// Snapshots share tree nodes; allocation updates copy only a balanced search path.
#[derive(Clone, Default)]
pub struct AllocationMap {
    root: Link,
    len: usize,
}

impl AllocationMap {
    pub(crate) fn additional_bytes(&self, previous: &Self) -> usize {
        fn allocation(value: &Allocation) -> usize {
            size_of::<Allocation>()
                + 6 * size_of::<usize>()
                + value.type_name.len()
                + value.file.len()
        }
        fn find<'a>(mut link: Option<&'a Arc<Node>>, key: &Key) -> Option<&'a Arc<Node>> {
            while let Some(node) = link {
                link = match key.cmp(&node.key) {
                    std::cmp::Ordering::Less => node.left.as_ref(),
                    std::cmp::Ordering::Greater => node.right.as_ref(),
                    std::cmp::Ordering::Equal => return Some(node),
                };
            }
            None
        }
        fn visit(
            current: Option<&Arc<Node>>,
            previous: Option<&Arc<Node>>,
            previous_root: Option<&Arc<Node>>,
        ) -> usize {
            let Some(node) = current else { return 0 };
            let prior = previous
                .filter(|other| other.key == node.key)
                .or_else(|| find(previous_root, &node.key));
            if prior.is_some_and(|other| Arc::ptr_eq(node, other)) {
                return 0;
            }
            size_of::<Node>()
                + 2 * size_of::<usize>()
                + if prior.is_some_and(|other| Arc::ptr_eq(&node.value, &other.value)) {
                    0
                } else {
                    allocation(&node.value)
                }
                + visit(
                    node.left.as_ref(),
                    prior.and_then(|other| other.left.as_ref()),
                    previous_root,
                )
                + visit(
                    node.right.as_ref(),
                    prior.and_then(|other| other.right.as_ref()),
                    previous_root,
                )
        }
        let previous_root = previous.root.as_ref();
        visit(self.root.as_ref(), previous_root, previous_root)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, key: &Key) -> Option<&Value> {
        let mut node = self.root.as_deref();
        while let Some(current) = node {
            node = match key.cmp(&current.key) {
                std::cmp::Ordering::Less => current.left.as_deref(),
                std::cmp::Ordering::Greater => current.right.as_deref(),
                std::cmp::Ordering::Equal => return Some(&current.value),
            };
        }
        None
    }

    pub fn get_index(&self, mut index: usize) -> Option<(&Key, &Value)> {
        let mut node = self.root.as_deref();
        while let Some(current) = node {
            let left = Node::len(&current.left);
            node = match index.cmp(&left) {
                std::cmp::Ordering::Less => current.left.as_deref(),
                std::cmp::Ordering::Equal => return Some((&current.key, &current.value)),
                std::cmp::Ordering::Greater => {
                    index -= left + 1;
                    current.right.as_deref()
                }
            };
        }
        None
    }

    pub fn insert(&mut self, key: Key, value: Value) -> Option<Value> {
        let previous = Node::insert(&mut self.root, key, value);
        self.len += usize::from(previous.is_none());
        previous
    }

    pub fn remove(&mut self, key: &Key) -> Option<Value> {
        self.get(key)?;
        let previous = Node::remove(&mut self.root, key);
        self.len -= usize::from(previous.is_some());
        previous
    }

    pub fn iter(&self) -> AllocationIter<'_> {
        let mut iter = AllocationIter {
            front: Vec::new(),
            back: Vec::new(),
            remaining: self.len,
        };
        iter.push_front(self.root.as_deref());
        iter.push_back(self.root.as_deref());
        iter
    }

    pub fn values(&self) -> impl DoubleEndedIterator<Item = &Value> + ExactSizeIterator {
        self.iter().map(|(_, value)| value)
    }

    pub fn values_mut(
        &mut self,
    ) -> impl DoubleEndedIterator<Item = &mut Value> + ExactSizeIterator {
        fn collect<'a>(link: &'a mut Link, values: &mut Vec<&'a mut Value>) {
            if let Some(root) = link {
                let Node {
                    left, value, right, ..
                } = Arc::make_mut(root);
                collect(left, values);
                values.push(value);
                collect(right, values);
            }
        }
        let mut values = Vec::with_capacity(self.len);
        collect(&mut self.root, &mut values);
        values.into_iter()
    }
}

pub struct AllocationIter<'a> {
    front: Vec<&'a Node>,
    back: Vec<&'a Node>,
    remaining: usize,
}

impl<'a> AllocationIter<'a> {
    fn push_front(&mut self, mut node: Option<&'a Node>) {
        while let Some(current) = node {
            self.front.push(current);
            node = current.left.as_deref();
        }
    }

    fn push_back(&mut self, mut node: Option<&'a Node>) {
        while let Some(current) = node {
            self.back.push(current);
            node = current.right.as_deref();
        }
    }
}

impl<'a> Iterator for AllocationIter<'a> {
    type Item = (&'a Key, &'a Value);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let node = self.front.pop()?;
        self.push_front(node.right.as_deref());
        self.remaining -= 1;
        Some((&node.key, &node.value))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl DoubleEndedIterator for AllocationIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let node = self.back.pop()?;
        self.push_back(node.left.as_deref());
        self.remaining -= 1;
        Some((&node.key, &node.value))
    }
}

impl ExactSizeIterator for AllocationIter<'_> {}
impl FusedIterator for AllocationIter<'_> {}

impl Index<&Key> for AllocationMap {
    type Output = Value;
    fn index(&self, key: &Key) -> &Self::Output {
        self.get(key).expect("allocation key is missing")
    }
}

impl PartialEq for AllocationMap {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len
            && (self
                .root
                .as_ref()
                .zip(other.root.as_ref())
                .is_some_and(|(left, right)| Arc::ptr_eq(left, right))
                || self.iter().eq(other.iter()))
    }
}
impl Eq for AllocationMap {}

impl PartialEq<BTreeMap<Key, Value>> for AllocationMap {
    fn eq(&self, other: &BTreeMap<Key, Value>) -> bool {
        self.len == other.len() && self.iter().eq(other.iter())
    }
}

impl PartialEq<AllocationMap> for BTreeMap<Key, Value> {
    fn eq(&self, other: &AllocationMap) -> bool {
        other == self
    }
}

impl fmt::Debug for AllocationMap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl FromIterator<(Key, Value)> for AllocationMap {
    fn from_iter<T: IntoIterator<Item = (Key, Value)>>(iter: T) -> Self {
        let mut map = Self::default();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl<'a> IntoIterator for &'a AllocationMap {
    type Item = (&'a Key, &'a Value);
    type IntoIter = AllocationIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for AllocationMap {
    type Item = (Key, Value);
    type IntoIter = std::vec::IntoIter<Self::Item>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
            .map(|(&key, value)| (key, value.clone()))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

impl Serialize for AllocationMap {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len))?;
        for (key, value) in self {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for AllocationMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        BTreeMap::<Key, Value>::deserialize(deserializer).map(|map| map.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allocation(index: u64) -> Value {
        Arc::new(Allocation {
            address: index,
            size: index + 1,
            type_name: "Resident".into(),
            file: "probe.cpp".into(),
            line: 1,
            received_frame: index as usize,
        })
    }

    fn verify(map: &AllocationMap, expected: &BTreeMap<Key, Value>) {
        fn height(link: &Link) -> u8 {
            let Some(node) = link else {
                return 0;
            };
            let left = height(&node.left);
            let right = height(&node.right);
            assert!(left.abs_diff(right) <= 1);
            assert_eq!(node.height, 1 + left.max(right));
            assert_eq!(
                node.subtree_len,
                1 + Node::len(&node.left) + Node::len(&node.right)
            );
            node.height
        }
        height(&map.root);
        assert_eq!(map.len(), expected.len());
        assert!(map.iter().eq(expected.iter()));
        assert!(map.iter().rev().eq(expected.iter().rev()));
        for (index, entry) in expected.iter().enumerate() {
            assert_eq!(map.get_index(index), Some(entry));
        }
        assert!(map.get_index(map.len()).is_none());
        assert!(map.get_index(usize::MAX).is_none());
        let mut actual = map.iter();
        let mut oracle = expected.iter();
        while actual.len() > 0 {
            assert_eq!(actual.next(), oracle.next());
            assert_eq!(actual.next_back(), oracle.next_back());
        }
        assert!(actual.next().is_none());
        assert!(actual.next_back().is_none());
    }

    #[test]
    fn rotations_charge_only_unshared_nodes_and_allocations() {
        fn pointers(link: &Link, nodes: &mut std::collections::BTreeSet<usize>) {
            if let Some(node) = link {
                nodes.insert(Arc::as_ptr(node) as usize);
                pointers(&node.left, nodes);
                pointers(&node.right, nodes);
            }
        }
        let mut map: AllocationMap = (0..11)
            .map(|index| ((1, index), allocation(index)))
            .collect();
        let held = map.clone();
        let mut before = std::collections::BTreeSet::new();
        pointers(&held.root, &mut before);
        let value = allocation(11);
        let allocation_bytes = size_of::<Allocation>()
            + 6 * size_of::<usize>()
            + value.type_name.len()
            + value.file.len();
        map.insert((1, 11), value);
        assert_ne!(
            map.root.as_ref().unwrap().key,
            held.root.as_ref().unwrap().key
        );
        let mut after = std::collections::BTreeSet::new();
        pointers(&map.root, &mut after);
        assert!(!before.is_disjoint(&after));
        assert_eq!(
            map.additional_bytes(&held),
            after.difference(&before).count() * (size_of::<Node>() + 2 * size_of::<usize>())
                + allocation_bytes
        );
        assert_eq!(held.len(), 11);
        assert!(held.get(&(1, 11)).is_none());
    }

    #[test]
    fn random_changes_and_held_snapshots_match_ordered_map() {
        let mut map = AllocationMap::default();
        let mut expected = BTreeMap::new();
        let mut held = Vec::new();
        let mut random = 43u64;
        for index in 0..6000 {
            random = random
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let key = ((random >> 20) % 3, (random >> 32) % 1024);
            if index % 3 == 0 {
                assert_eq!(map.remove(&key), expected.remove(&key));
            } else {
                let value = allocation(index);
                assert_eq!(map.insert(key, value.clone()), expected.insert(key, value));
            }
            assert_eq!(map.get(&key), expected.get(&key));
            if index % 173 == 0 {
                verify(&map, &expected);
                held.push((map.clone(), expected.clone()));
                if held.len() > 8 {
                    held.remove(0);
                }
                for (snapshot, oracle) in &held {
                    verify(snapshot, oracle);
                }
            }
        }
        for key in expected.keys().copied().collect::<Vec<_>>() {
            assert_eq!(map.remove(&key), expected.remove(&key));
        }
        verify(&map, &expected);
        for (snapshot, oracle) in held {
            verify(&snapshot, &oracle);
        }
    }

    #[test]
    fn ordered_growth_removal_and_mutable_values_preserve_snapshots_and_wire_bytes() {
        let expected: BTreeMap<_, _> = (0..4096)
            .map(|index| ((1, index), allocation(index)))
            .collect();
        let mut map: AllocationMap = expected.clone().into_iter().collect();
        verify(&map, &expected);
        let bytes = postcard::to_stdvec(&map).unwrap();
        assert_eq!(bytes, postcard::to_stdvec(&expected).unwrap());
        let decoded: AllocationMap = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(map, decoded);
        let held = map.clone();
        fn pointers(link: &Link, nodes: &mut std::collections::BTreeSet<usize>) {
            if let Some(node) = link {
                nodes.insert(Arc::as_ptr(node) as usize);
                pointers(&node.left, nodes);
                pointers(&node.right, nodes);
            }
        }
        let mut before = std::collections::BTreeSet::new();
        pointers(&held.root, &mut before);
        map.insert((1, 5000), allocation(5000));
        let mut after = std::collections::BTreeSet::new();
        pointers(&map.root, &mut after);
        assert!(before.intersection(&after).count() >= held.len() - 32);
        assert_eq!(held.additional_bytes(&held.clone()), 0);
        assert!(
            map.additional_bytes(&held)
                >= after.difference(&before).count() * (size_of::<Node>() + 2 * size_of::<usize>())
        );
        assert!(map.additional_bytes(&held) < 8192);
        map.remove(&(1, 5000));
        verify(&map, &expected);
        for value in map.values_mut() {
            Arc::make_mut(value).line = 2;
        }
        assert!(held.values().all(|value| value.line == 1));
        assert!(map.values().all(|value| value.line == 2));
        let mut remaining = expected.clone();
        let mut decoded = decoded;
        for key in expected.keys().rev() {
            assert_eq!(decoded.remove(key), remaining.remove(key));
            if decoded.len().is_multiple_of(137) {
                verify(&decoded, &remaining);
            }
        }
        verify(&decoded, &remaining);
        verify(&held, &expected);
        assert!(
            std::mem::size_of::<Node>() + 2 * std::mem::size_of::<usize>()
                <= super::super::LIVE_ALLOCATION_ENTRY_BYTES
        );
    }
}
