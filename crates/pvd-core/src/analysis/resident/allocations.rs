use super::*;
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(crate) struct ResidentMap {
    pub(super) root: ResidentLink,
    pub(super) len: usize,
    pub(super) domain: Domain,
}
impl ResidentMap {
    pub(crate) fn iter(&self) -> ResidentIter<'_> {
        let mut iter = ResidentIter {
            stack: [None; MAX_AVL_HEIGHT as usize],
            len: 0,
        };
        iter.left(self.root.as_deref());
        iter
    }
    pub(crate) fn empty(domain: Domain) -> Self {
        Self {
            root: None,
            len: 0,
            domain,
        }
    }
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn get(&self, key: &Key) -> Option<&ResidentValue> {
        let mut next = self.root.as_ref();
        while let Some(node) = next {
            match key.cmp(&node.key) {
                std::cmp::Ordering::Less => next = node.left.as_ref(),
                std::cmp::Ordering::Greater => next = node.right.as_ref(),
                std::cmp::Ordering::Equal => return Some(&node.value),
            }
        }
        None
    }
    pub(crate) fn get_index(&self, mut index: usize) -> Option<(Key, &ResidentValue)> {
        if index >= self.len {
            return None;
        }
        let mut next = self.root.as_deref();
        while let Some(node) = next {
            let left = ResidentNode::count(&node.left) as usize;
            match index.cmp(&left) {
                std::cmp::Ordering::Less => next = node.left.as_deref(),
                std::cmp::Ordering::Equal => return Some((node.key, &node.value)),
                std::cmp::Ordering::Greater => {
                    index -= left + 1;
                    next = node.right.as_deref();
                }
            }
        }
        None
    }
    pub(crate) fn try_insert(
        &mut self,
        key: Key,
        value: ResidentValue,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<ResidentValue>, Error> {
        check_replay_cancelled(cancelled)?;
        if !value.0.domain().same(&self.domain) {
            return Err(error("resident map cannot adopt a foreign value"));
        }
        if self.len >= 1_000_000 && self.get(&key).is_none() {
            return Err(error("resident map exceeds its entry limit"));
        }
        let mut draft = self.root.clone();
        let replaced = ResidentNode::insert(&mut draft, key, value, &self.domain, cancelled)?;
        let len = if replaced.is_some() {
            self.len
        } else {
            add(self.len, 1)?
        };
        check_replay_cancelled(cancelled)?;
        self.root = draft;
        self.len = len;
        Ok(replaced)
    }
    pub(crate) fn try_remove(
        &mut self,
        key: &Key,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<ResidentValue>, Error> {
        check_replay_cancelled(cancelled)?;
        if self.get(key).is_none() {
            return Ok(None);
        }
        let mut draft = self.root.clone();
        let removed = ResidentNode::remove(&mut draft, key, &self.domain, cancelled)?;
        check_replay_cancelled(cancelled)?;
        self.root = draft;
        self.len -= usize::from(removed.is_some());
        Ok(removed)
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        fn visit(
            link: &ResidentLink,
            lower: Option<Key>,
            upper: Option<Key>,
        ) -> Result<(usize, u8), Error> {
            let Some(node) = link else {
                return Ok((0, 0));
            };
            if lower.is_some_and(|key| node.key <= key) || upper.is_some_and(|key| node.key >= key)
            {
                return Err(error("resident key bounds invalid"));
            }
            let (lc, lh) = visit(&node.left, lower, Some(node.key))?;
            let (rc, rh) = visit(&node.right, Some(node.key), upper)?;
            if lh.abs_diff(rh) > 1 || node.height != 1 + lh.max(rh) {
                return Err(error("resident AVL height invalid"));
            }
            let count = add(add(lc, rc)?, 1)?;
            if node.subtree_len as usize != count {
                return Err(error("resident subtree rank disagrees"));
            }
            Ok((count, node.height))
        }
        if visit(&self.root, None, None)?.0 != self.len {
            return Err(error("resident count invalid"));
        }
        Ok(())
    }
}

pub(crate) struct ResidentIter<'a> {
    stack: [Option<&'a ResidentNode>; MAX_AVL_HEIGHT as usize],
    len: usize,
}
impl<'a> ResidentIter<'a> {
    fn left(&mut self, mut next: Option<&'a ResidentNode>) {
        while let Some(node) = next {
            self.stack[self.len] = Some(node);
            self.len += 1;
            next = node.left.as_deref();
        }
    }
}
impl<'a> Iterator for ResidentIter<'a> {
    type Item = (Key, &'a ResidentValue);
    fn next(&mut self) -> Option<Self::Item> {
        self.len = self.len.checked_sub(1)?;
        let node = self.stack[self.len].take().unwrap();
        self.left(node.right.as_deref());
        Some((node.key, &node.value))
    }
}
impl Serialize for ResidentMap {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.len))?;
        for (key, value) in self.iter() {
            map.serialize_entry(&key, value)?;
        }
        map.end()
    }
}

impl ResidentNode {
    fn count(link: &ResidentLink) -> u32 {
        link.as_ref().map_or(0, |node| node.subtree_len)
    }
    fn height(link: &ResidentLink) -> u8 {
        link.as_ref().map_or(0, |node| node.height)
    }
    fn refresh(&mut self) -> Result<(), Error> {
        self.height = 1 + Self::height(&self.left).max(Self::height(&self.right));
        self.subtree_len = Self::count(&self.left)
            .checked_add(Self::count(&self.right))
            .and_then(|count| count.checked_add(1))
            .ok_or_else(|| error("resident subtree rank overflow"))?;
        Ok(())
    }
    fn balance(&self) -> i16 {
        i16::from(Self::height(&self.left)) - i16::from(Self::height(&self.right))
    }
    fn rotate_left(
        root: &mut Owned<Self>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut pivot = root
            .make_mut()?
            .right
            .take()
            .ok_or_else(|| error("resident rotation lacks pivot"))?;
        root.make_mut()?.right = pivot.make_mut()?.left.take();
        root.make_mut()?.refresh()?;
        pivot.make_mut()?.left = Some(root.clone());
        pivot.make_mut()?.refresh()?;
        *root = pivot;
        Ok(())
    }
    fn rotate_right(
        root: &mut Owned<Self>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
        check_replay_cancelled(cancelled)?;
        let mut pivot = root
            .make_mut()?
            .left
            .take()
            .ok_or_else(|| error("resident rotation lacks pivot"))?;
        root.make_mut()?.left = pivot.make_mut()?.right.take();
        root.make_mut()?.refresh()?;
        pivot.make_mut()?.right = Some(root.clone());
        pivot.make_mut()?.refresh()?;
        *root = pivot;
        Ok(())
    }
    fn rebalance(
        root: &mut Owned<Self>,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<(), Error> {
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
        link: &mut ResidentLink,
        key: Key,
        value: ResidentValue,
        domain: &Domain,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<ResidentValue>, Error> {
        check_replay_cancelled(cancelled)?;
        let Some(root) = link else {
            *link = Some(Owned::new(
                Self {
                    key,
                    value,
                    left: None,
                    right: None,
                    subtree_len: 1,
                    height: 1,
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
        link: &mut ResidentLink,
        key: &Key,
        domain: &Domain,
        cancelled: &mut impl FnMut() -> bool,
    ) -> Result<Option<ResidentValue>, Error> {
        check_replay_cancelled(cancelled)?;
        let Some(root) = link.as_mut() else {
            return Ok(None);
        };
        if !root.domain().same(domain) {
            return Err(error("resident subtree has a foreign budget"));
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
