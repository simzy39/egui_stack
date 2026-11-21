use std::{
    ops,
    sync::atomic::{AtomicU64, Ordering},
};

use egui::Rect;

use crate::{Error, Result, TabIndex};

static NEXT_LEAF_DRAG_ID: AtomicU64 = AtomicU64::new(1);

/// The inner data of a [``Node::Leaf``](crate::Node), which contains tabs and can be collapsed.
#[derive(Debug)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize, serde::Serialize),
    serde(default)
)]
pub struct LeafNode<Tab> {
    /// The full rectangle - tab bar plus tab body.
    pub rect: Rect,

    /// The tab body rectangle.
    pub viewport: Rect,

    /// All the tabs in this node.
    pub tabs: Vec<Tab>,

    /// The opened tab.
    pub active: TabIndex,

    /// Scroll amount of the tab bar.
    pub scroll: f32,

    /// Whether the leaf is collapsed.
    pub collapsed: bool,

    /// Whether the leaf tab bar is hidden.
    pub tab_bar_hidden: bool,

    /// Runtime-only identity; zero means unassigned.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) drag_identity: u64,
}

impl<Tab: Clone> Clone for LeafNode<Tab> {
    fn clone(&self) -> Self {
        Self {
            rect: self.rect,
            viewport: self.viewport,
            tabs: self.tabs.clone(),
            active: self.active,
            scroll: self.scroll,
            collapsed: self.collapsed,
            tab_bar_hidden: self.tab_bar_hidden,
            drag_identity: 0,
        }
    }
}

fn allocate_drag_identity(counter: &AtomicU64) -> u64 {
    counter
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            (next != 0).then(|| next.checked_add(1)).flatten()
        })
        .unwrap_or_else(|_| panic!("leaf drag identity exhausted"))
}

impl<Tab> LeafNode<Tab> {
    pub(crate) fn ensure_drag_identity(&mut self) -> u64 {
        if self.drag_identity == 0 {
            self.drag_identity = allocate_drag_identity(&NEXT_LEAF_DRAG_ID);
        }
        self.drag_identity
    }

    /// Create New LeafNode with specified ``tabs``, all other internal values will be filled by "nothing" defaults.
    pub const fn new(tabs: Vec<Tab>) -> Self {
        LeafNode {
            rect: Rect::NOTHING,
            viewport: Rect::NOTHING,
            tabs,
            active: TabIndex(0),
            scroll: 0.0,
            collapsed: false,
            tab_bar_hidden: false,
            drag_identity: 0,
        }
    }

    /// Set the active tab of this [`LeafNode`]
    ///
    /// If `active_tab` is out of bounds, an error is returned and the active tab is not changed.
    #[inline]
    pub fn set_active_tab(&mut self, active_tab: impl Into<TabIndex>) -> Result {
        let index = active_tab.into();
        if index.0 < self.len() {
            self.active = index;
            Ok(())
        } else {
            Err(Error::InvalidTab)
        }
    }

    /// Set the area this [`LeafNode`] Occupies on screen.
    #[inline]
    pub fn set_rect(&mut self, new_rect: Rect) {
        self.rect = new_rect;
    }

    /// Get the length of tab list in this [`LeafNode`].
    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Returns `true` when the [`LeafNode`] contains no tabs.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Get a [`Rect`] representing the area this [`LeafNode`] occupies on screen.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Get immutable access to the ``Tab``s of this [`LeafNode`]
    #[inline]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Get mutable access to the ``Tab``s of this [`LeafNode`]
    #[inline]
    pub fn tabs_mut(&mut self) -> &mut [Tab] {
        &mut self.tabs
    }

    /// Append a ``Tab`` to the end of this [`LeafNode`]s tab list.
    ///
    /// This will also focus the added tab.
    #[track_caller]
    #[inline]
    pub fn append_tab(&mut self, tab: Tab) {
        self.active = TabIndex(self.tabs.len());
        self.tabs.push(tab);
    }

    /// Insert a ``Tab`` to this [`LeafNode`]s tab list at the specified [`TabIndex`].
    ///
    /// This will also focus the added tab.
    ///
    /// # Panics
    ///
    /// if ``tab_index`` exceeds the leaf's tab list length.
    #[track_caller]
    #[inline]
    pub fn insert_tab(&mut self, tab_index: impl Into<TabIndex>, tab: Tab) {
        let tab_index = tab_index.into();
        self.tabs.insert(tab_index.0, tab);
        self.active = tab_index;
    }

    /// Remove the tab at `tab_index`, returning `None` when out of bounds.
    /// An invalid index leaves both the tabs and active selection unchanged.
    #[inline]
    pub fn remove_tab(&mut self, tab_index: impl Into<TabIndex>) -> Option<Tab> {
        let index = tab_index.into();
        if index.0 >= self.tabs.len() {
            return None;
        }
        if index <= self.active {
            self.active.0 = self.active.0.saturating_sub(1);
        }
        Some(self.tabs.remove(index.0))
    }

    /// Removes all tabs for which `predicate` returns `false`.
    pub fn retain_tabs<F>(&mut self, predicate: F)
    where
        F: FnMut(&mut Tab) -> bool,
    {
        self.tabs.retain_mut(predicate);
    }

    /// Return the area and tab which is currently representing this [`LeafNode`]
    ///
    /// This may return ``None`` if the leaf contains 0 tabs.
    #[inline]
    pub fn active_focused(&mut self) -> Option<(Rect, &mut Tab)> {
        self.tabs
            .get_mut(self.active.0)
            .map(|tab| (self.viewport, tab))
    }
}

impl<Tab> Default for LeafNode<Tab> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl<Tab> ops::Index<TabIndex> for LeafNode<Tab> {
    type Output = Tab;

    fn index(&self, index: TabIndex) -> &Tab {
        &self.tabs[index.0]
    }
}

impl<Tab> ops::IndexMut<TabIndex> for LeafNode<Tab> {
    fn index_mut(&mut self, index: TabIndex) -> &mut Tab {
        &mut self.tabs[index.0]
    }
}

#[cfg(test)]
mod removal_tests {
    use super::*;

    #[test]
    fn invalid_removal_preserves_tabs_and_active_selection() {
        let mut leaf = LeafNode::new(vec!["first", "second"]);
        leaf.active = TabIndex(1);
        assert_eq!(leaf.remove_tab(TabIndex(2)), None);
        assert_eq!(leaf.remove_tab(TabIndex(usize::MAX)), None);
        assert_eq!(leaf.tabs, ["first", "second"]);
        assert_eq!(leaf.active, TabIndex(1));
        assert_eq!(leaf.remove_tab(TabIndex(0)), Some("first"));
        assert_eq!(leaf.tabs, ["second"]);
        assert_eq!(leaf.active, TabIndex(0));
    }
}

#[cfg(test)]
mod drag_identity_tests {
    use super::*;

    #[test]
    fn allocator_returns_unique_nonzero_identities() {
        let counter = AtomicU64::new(1);
        assert_eq!(allocate_drag_identity(&counter), 1);
        assert_eq!(allocate_drag_identity(&counter), 2);
        assert_eq!(counter.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn allocator_exhaustion_never_wraps_or_reuses_an_identity() {
        let counter = AtomicU64::new(u64::MAX - 1);
        assert_eq!(allocate_drag_identity(&counter), u64::MAX - 1);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);

        let exhausted = std::panic::catch_unwind(|| allocate_drag_identity(&counter));
        assert!(exhausted.is_err());
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }

    #[test]
    fn allocator_rejects_the_unassigned_zero_sentinel() {
        let counter = AtomicU64::new(0);
        let exhausted = std::panic::catch_unwind(|| allocate_drag_identity(&counter));
        assert!(exhausted.is_err());
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cloned_leaves_receive_distinct_runtime_identities() {
        let mut original = LeafNode::new(vec!["tab"]);
        let original_id = original.ensure_drag_identity();
        let mut cloned = original.clone();
        let cloned_id = cloned.ensure_drag_identity();

        assert_ne!(original_id, cloned_id);
        assert_eq!(original.ensure_drag_identity(), original_id);
        assert_eq!(cloned.ensure_drag_identity(), cloned_id);
    }
}
