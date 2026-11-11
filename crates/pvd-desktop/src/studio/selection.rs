use super::{Message, Studio};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct Selection {
    objects: BTreeMap<u64, u32>,
    primary: Option<u64>,
    revision: u64,
}

impl Studio {
    pub fn is_selected(&self, id: u64) -> bool {
        if self.selected != self.selection.primary {
            return self.selected == Some(id);
        }
        self.selection.objects.get(&id).is_some_and(|generation| {
            self.state
                .objects
                .get(&id)
                .is_some_and(|object| object.generation == *generation)
        })
    }

    pub fn selected_ids(&self) -> BTreeSet<u64> {
        if self.selected != self.selection.primary {
            return self
                .selected
                .filter(|id| self.state.objects.contains_key(id))
                .into_iter()
                .collect();
        }
        self.selection
            .objects
            .keys()
            .copied()
            .filter(|id| self.is_selected(*id))
            .collect()
    }

    pub fn selection_revision(&self) -> u64 {
        self.selection.revision
    }

    pub fn select_object(&mut self, id: Option<u64>, toggle: bool) {
        self.refresh_selection();
        let Some(id) = id else {
            if !toggle {
                self.clear_selection();
            }
            return;
        };
        let Some(object) = self.state.objects.get(&id) else {
            return;
        };
        let generation = object.generation;
        if toggle && self.selection.objects.remove(&id).is_some() {
            if self.selected == Some(id) {
                self.selected = self.selection.objects.keys().next_back().copied();
            }
        } else {
            if toggle && self.selection.objects.len() == pvd_core::repx::MAX_SELECTED_ACTORS {
                self.notice(Message::new(
                    "Selection limit reached (4096 objects).",
                    "最多选择 4096 个对象。",
                ));
                return;
            }
            if !toggle {
                if self.selection.objects.len() == 1 && self.selected == Some(id) {
                    return;
                }
                self.selection.objects.clear();
            }
            self.selection.objects.insert(id, generation);
            self.selected = Some(id);
        }
        self.selection.primary = self.selected;
        self.selection.revision = self.selection.revision.wrapping_add(1);
    }

    pub(super) fn clear_selection(&mut self) {
        if self.selected.is_some() || !self.selection.objects.is_empty() {
            self.selection.objects.clear();
            self.selection.revision = self.selection.revision.wrapping_add(1);
        }
        self.selected = None;
        self.selection.primary = None;
    }

    pub(super) fn refresh_selection(&mut self) {
        if self.selected != self.selection.primary {
            self.selection.objects.clear();
            if let Some(object) = self.selected.and_then(|id| self.state.objects.get(&id)) {
                self.selection.objects.insert(object.id, object.generation);
            }
            self.selection.primary = self.selected;
            self.selection.revision = self.selection.revision.wrapping_add(1);
        }
        let count = self.selection.objects.len();
        self.selection.objects.retain(|id, generation| {
            self.state
                .objects
                .get(id)
                .is_some_and(|object| object.generation == *generation)
        });
        let primary = self.selected;
        if self
            .selected
            .is_some_and(|id| !self.selection.objects.contains_key(&id))
        {
            self.selected = self.selection.objects.keys().next_back().copied();
        }
        if count != self.selection.objects.len() || primary != self.selected {
            self.selection.revision = self.selection.revision.wrapping_add(1);
        }
        self.selection.primary = self.selected;
    }

    pub fn repx_selection(&self) -> pvd_core::repx::Selection {
        pvd_core::repx::Selection::Actors(
            self.selected_ids()
                .into_iter()
                .map(|id| pvd_core::repx::ActorKey {
                    id,
                    generation: self.state.objects[&id].generation,
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests;
