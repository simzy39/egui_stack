use std::ops;

use egui::{Pos2, Rect, Vec2};

use crate::{TabViewer, tab_viewer::OnCloseResponse};

mod error;
pub use error::{Error, Result};

/// Wrapper around indices to the collection of surfaces inside a [`DockState`].
pub mod surface_index;

pub mod tree;

/// Represents an area in which a dock tree is rendered.
pub mod surface;
/// Specifies text displayed in different elements of the [`DockArea`](crate::DockArea).
pub mod translations;
/// Window states which tells floating tabs how to be displayed inside their window,
pub mod window_state;

pub use surface::Surface;
pub use surface_index::SurfaceIndex;
use tree::node::LeafNode;
pub use window_state::WindowState;

use crate::{
    Node, NodeIndex, NodePath, Split, TabDestination, TabIndex, TabInsert, TabPath, Translations,
    Tree,
};

/// The heart of `egui_stack`.
///
/// This structure holds a collection of surfaces, each of which stores a tree in which tabs are arranged.
///
/// Indexing it with a [`SurfaceIndex`] will yield a [`Tree`] which then contains nodes and tabs.
///
/// [`DockState`] is generic, so you can use any type of data to represent a tab.
///
/// # Serialization
///
/// `DockState` may be serialized to record the placement of all surfaces.
///
/// This does not include serialization of translations.
#[derive(Debug)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct DockState<Tab> {
    surfaces: Vec<SurfaceSlot<Tab>>,
    focused_surface: Option<SurfaceIndex>, // Part of the tree which is in focus.

    /// Contains translations of text shown in [`DockArea`](crate::DockArea).
    #[cfg_attr(feature = "serde", serde(skip))]
    pub translations: Translations,
}

/// A surface and its runtime identity form one owned slot. The identity is
/// deliberately excluded from persistence and reset on cloning.
#[derive(Debug)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize, serde::Serialize),
    serde(transparent)
)]
struct SurfaceSlot<Tab> {
    surface: Surface<Tab>,
    #[cfg_attr(feature = "serde", serde(skip))]
    identity: u64,
}

impl<Tab> SurfaceSlot<Tab> {
    fn new(surface: Surface<Tab>) -> Self {
        Self { surface, identity: 0 }
    }
}

impl<Tab: Clone> Clone for SurfaceSlot<Tab> {
    fn clone(&self) -> Self {
        Self::new(self.surface.clone())
    }
}

impl<Tab> std::ops::Deref for SurfaceSlot<Tab> {
    type Target = Surface<Tab>;

    fn deref(&self) -> &Self::Target {
        &self.surface
    }
}

impl<Tab> std::ops::DerefMut for SurfaceSlot<Tab> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.identity = 0;
        &mut self.surface
    }
}

impl<Tab: Clone> Clone for DockState<Tab> {
    fn clone(&self) -> Self {
        Self {
            surfaces: self.surfaces.clone(),
            focused_surface: self.focused_surface,
            translations: self.translations.clone(),
        }
    }
}

impl<Tab> ops::Index<SurfaceIndex> for DockState<Tab> {
    type Output = Tree<Tab>;

    #[inline(always)]
    fn index(&self, index: SurfaceIndex) -> &Self::Output {
        match self.surfaces[index.0].node_tree() {
            Some(tree) => tree,
            None => {
                panic!("There did not exist a tree at surface index {}", index.0);
            }
        }
    }
}

impl<Tab> ops::IndexMut<SurfaceIndex> for DockState<Tab> {
    #[inline(always)]
    fn index_mut(&mut self, index: SurfaceIndex) -> &mut Self::Output {
        self.invalidate_surface_identity(index);
        match self.surfaces[index.0].node_tree_mut() {
            Some(tree) => tree,
            None => {
                panic!("There did not exist a tree at surface index {}", index.0);
            }
        }
    }
}

impl<Tab> ops::Index<NodePath> for DockState<Tab> {
    type Output = Node<Tab>;

    #[inline(always)]
    fn index(&self, index: NodePath) -> &Self::Output {
        match self.surfaces[index.surface.0].node_tree() {
            Some(tree) => &tree[index.node],
            None => {
                panic!(
                    "There did not exist a tree at surface index {}",
                    index.surface.0
                );
            }
        }
    }
}

impl<Tab> ops::IndexMut<NodePath> for DockState<Tab> {
    #[inline(always)]
    fn index_mut(&mut self, index: NodePath) -> &mut Self::Output {
        self.invalidate_surface_identity(index.surface);
        match self.surfaces[index.surface.0].node_tree_mut() {
            Some(tree) => &mut tree[index.node],
            None => {
                panic!(
                    "There did not exist a tree at surface index {}",
                    index.surface.0
                );
            }
        }
    }
}

impl<Tab> DockState<Tab> {
    /// Create a new tree with given tabs at the main surface's root node.
    pub fn new(tabs: Vec<Tab>) -> Self {
        Self {
            surfaces: vec![SurfaceSlot::new(Surface::Main(Tree::new(tabs)))],
            focused_surface: None,
            translations: Translations::english(),
        }
    }

    /// Sets translations of text later displayed in [`DockArea`](crate::DockArea).
    pub fn with_translations(mut self, translations: Translations) -> Self {
        self.translations = translations;
        self
    }

    /// Get an immutable borrow to the tree at the main surface.
    pub fn main_surface(&self) -> &Tree<Tab> {
        &self[SurfaceIndex::main()]
    }

    /// Get a mutable borrow to the tree at the main surface.
    pub fn main_surface_mut(&mut self) -> &mut Tree<Tab> {
        &mut self[SurfaceIndex::main()]
    }

    /// Get the [`WindowState`] which corresponds to a [`SurfaceIndex`].
    ///
    /// Returns `None` if the surface is [`Empty`](Surface::Empty), [`Main`](Surface::Main), or doesn't exist.
    ///
    /// This can be used to modify properties of a window, e.g. size and position.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use egui_stack::DockState;
    /// # use egui::{Vec2, Pos2};
    /// let mut dock_state = DockState::new(vec![]);
    /// let mut surface_index = dock_state.add_window(vec!["Window Tab".to_string()]);
    /// let window_state = dock_state.get_window_state_mut(surface_index).unwrap();
    ///
    /// window_state.set_position(Pos2::ZERO);
    /// window_state.set_size(Vec2::splat(100.0));
    /// ```
    pub fn get_window_state_mut(&mut self, surface: SurfaceIndex) -> Option<&mut WindowState> {
        match &mut self.surfaces.get_mut(surface.0)?.surface {
            Surface::Window(_, state) => Some(state),
            _ => None,
        }
    }

    /// Get the [`WindowState`] which corresponds to a [`SurfaceIndex`].
    ///
    /// Returns `None` if the surface is an [`Empty`](Surface::Empty), [`Main`](Surface::Main), or doesn't exist.
    pub fn get_window_state(&self, surface: SurfaceIndex) -> Option<&WindowState> {
        match &self.surfaces.get(surface.0)?.surface {
            Surface::Window(_, state) => Some(state),
            _ => None,
        }
    }

    /// Whether the surface at `surface` is a floating window, as opposed to the main surface or a
    /// dock of a [`DockSpace`](crate::DockSpace).
    #[inline]
    pub(crate) fn is_window(&self, surface: SurfaceIndex) -> bool {
        matches!(self.surfaces.get(surface.0).map(|slot| &slot.surface), Some(Surface::Window(..)))
    }

    /// Capture the identity of a surface slot, including a vacant slot, assigning one on first use.
    /// Identities are runtime-only; reused or replaced slots receive new identities.
    pub(crate) fn capture_surface_identity(&mut self, index: SurfaceIndex) -> Option<u64> {
        self.surfaces.get(index.0)?;
        let identity = &mut self.surfaces[index.0].identity;
        if *identity == 0 {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            *identity = NEXT
                .fetch_update(
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                    |next| (next != 0).then(|| next.checked_add(1)).flatten(),
                )
                .unwrap_or_else(|_| panic!("surface identity exhausted"));
        }
        Some(*identity)
    }

    /// Read an identity without assigning one to a replacement surface.
    pub(crate) fn current_surface_identity(&self, index: SurfaceIndex) -> Option<u64> {
        self.surfaces.get(index.0)?;
        Some(self.surfaces[index.0].identity).filter(|identity| *identity != 0)
    }

    fn invalidate_surface_identity(&mut self, index: SurfaceIndex) {
        if let Some(slot) = self.surfaces.get_mut(index.0) {
            slot.identity = 0;
        }
    }

    /// Puts `tree` in a docked surface ([`Surface::Main`]): the first empty slot after the main
    /// surface, or a new one at the end.
    pub(crate) fn add_docked_surface(&mut self, tree: Tree<Tab>) -> SurfaceIndex {
        let index = self.find_empty_surface_index();
        self.invalidate_surface_identity(index);
        let surface = Surface::Main(tree);
        if index.0 < self.surfaces.len() {
            self.surfaces[index.0] = SurfaceSlot::new(surface);
        } else {
            self.surfaces.push(SurfaceSlot::new(surface));
        }
        index
    }

    /// Takes the surface at `surface` out, leaving it empty: the main surface keeps an empty
    /// tree, any other becomes [`Surface::Empty`]. The focus leaves it.
    pub(crate) fn take_surface(&mut self, surface: SurfaceIndex) -> Option<Surface<Tab>> {
        let empty = if surface.is_main() {
            Surface::Main(Tree::new(vec![]))
        } else {
            Surface::Empty
        };
        let taken = std::mem::replace(&mut self.surfaces.get_mut(surface.0)?.surface, empty);
        self.invalidate_surface_identity(surface);
        if self.focused_surface == Some(surface) {
            self.focused_surface = None;
        }
        Some(taken)
    }

    /// The surfaces, by value.
    pub(crate) fn into_surfaces(self) -> Vec<Surface<Tab>> {
        self.surfaces.into_iter().map(|slot| slot.surface).collect()
    }

    /// Returns the viewport [`Rect`] and the `Tab` inside the focused leaf node or `None` if no node is in focus.
    #[inline]
    pub fn find_active_focused(&mut self) -> Option<(Rect, &mut Tab)> {
        self.focused_surface
            .and_then(|surface| self[surface].find_active_focused())
    }

    /// Get a mutable borrow to the raw surface from a surface index.
    #[inline]
    pub fn get_surface_mut(&mut self, surface: SurfaceIndex) -> Option<&mut Surface<Tab>> {
        self.invalidate_surface_identity(surface);
        self.surfaces.get_mut(surface.0).map(|slot| &mut slot.surface)
    }

    /// Get an immutable borrow to the raw surface from a surface index.
    #[inline]
    pub fn get_surface(&self, surface: SurfaceIndex) -> Option<&Surface<Tab>> {
        self.surfaces.get(surface.0).map(|slot| &slot.surface)
    }

    /// Returns true if the specified surface exists and isn't [`Empty`](Surface::Empty).
    #[inline]
    pub fn is_surface_valid(&self, surface_index: SurfaceIndex) -> bool {
        self.surfaces
            .get(surface_index.0)
            .is_some_and(|surface| !surface.is_empty())
    }

    /// Returns a list of all valid [`SurfaceIndex`]es.
    #[inline]
    pub(crate) fn valid_surface_indices(&self) -> Box<[SurfaceIndex]> {
        (0..self.surfaces.len())
            .filter_map(|index| {
                let index = SurfaceIndex(index);
                self.is_surface_valid(index).then_some(index)
            })
            .collect()
    }

    /// Remove a surface based on its [`SurfaceIndex`]
    ///
    /// Returns the removed surface or `None` if it didn't exist.
    ///
    /// Returns `None` for the main surface, a vacant slot, or an invalid index.
    /// Repeated removals are idempotent and cannot remove a later surface that
    /// happens to reuse the same slot.
    pub fn remove_surface(&mut self, surface_index: SurfaceIndex) -> Option<Surface<Tab>> {
        if surface_index.is_main()
            || !self.is_surface_valid(surface_index)
        {
            return None;
        }
        if self.focused_surface == Some(surface_index) {
            self.focused_surface = Some(SurfaceIndex::main());
        }
        self.invalidate_surface_identity(surface_index);
        if surface_index.0 == self.surfaces.len() - 1 {
            self.surfaces.pop().map(|slot| slot.surface)
        } else {
            Some(std::mem::replace(
                &mut self.surfaces[surface_index.0].surface,
                Surface::Empty,
            ))
        }
    }

    /// Sets which is the active tab at a specific `path`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if `path.surface` is not a valid surface,
    /// if the node at `path.node` is not a leaf or doesn't exist,
    /// or if the tab index at `path.tab` doesn't exist within the leaf node.
    #[inline]
    pub fn set_active_tab(&mut self, path: TabPath) -> Result {
        // Check the tab before borrowing the leaf mutably. An invalid tab
        // index must not invalidate an otherwise unchanged surface slot.
        self.leaf(path.node_path())?
            .tabs
            .get(path.tab.0)
            .ok_or(Error::InvalidTab)?;
        self.leaf_mut(path.node_path())?
            .set_active_tab(path.tab)?;
        Ok(())
    }

    /// Immutably borrows a node at the given `path`.
    ///
    /// This is the same as `&self[path]` but returns an error instead of panicking.
    pub fn node(&self, path: NodePath) -> Result<&Node<Tab>> {
        self.surfaces
            .get(path.surface.0)
            .ok_or(Error::InvalidSurface)?
            .node_tree()
            .ok_or(Error::EmptySurface)?
            .nodes
            .get(path.node.0)
            .ok_or(Error::InvalidNode)
    }

    /// Mutably borrows a node at the given `path`.
    ///
    /// This is the same as `&mut self[path]` but returns an error instead of panicking.
    pub fn node_mut(&mut self, path: NodePath) -> Result<&mut Node<Tab>> {
        // Validate before invalidating the slot. A rejected mutable lookup is
        // not a mutation and must not discard a retained surface identity.
        self.node(path)?;
        self.invalidate_surface_identity(path.surface);
        Ok(&mut self.surfaces[path.surface.0]
            .node_tree_mut()
            .expect("validated surface tree")
            .nodes[path.node.0])
    }

    /// Immutably borrows a leaf node at the given `path`.
    ///
    /// Returns `Err` if the `path` is invalid or the node at the path is not a leaf.
    pub fn leaf(&self, path: NodePath) -> Result<&LeafNode<Tab>> {
        self.node(path)?.get_leaf().ok_or(Error::NonLeafNode)
    }

    /// Mutably borrows a leaf node at the given `path`.
    ///
    /// Returns `Err` if the `path` is invalid or the node at the `path` is not a leaf.
    pub fn leaf_mut(&mut self, path: NodePath) -> Result<&mut LeafNode<Tab>> {
        // A non-leaf lookup must not invalidate an otherwise unchanged slot.
        self.leaf(path)?;
        Ok(self.node_mut(path)?
            .get_leaf_mut()
            .expect("validated leaf node"))
    }

    /// Sets the currently focused leaf to `path` if the node at `path` is a leaf.
    #[inline]
    pub fn set_focused_node_and_surface(&mut self, path: NodePath) {
        if self.leaf(path).is_ok() {
            self.focused_surface = Some(path.surface);
            self[path.surface].set_focused_node(path.node);
        } else {
            self.focused_surface = None;
        }
    }

    /// Whether a destination can accept a move without clamping a stale tab index.
    pub(crate) fn valid_move_destination(&self, destination: &TabDestination) -> bool {
        match *destination {
            TabDestination::Node(path, TabInsert::Split(_)) => self
                .node(path)
                .is_ok_and(|node| !node.is_empty()),
            TabDestination::Node(path, TabInsert::Insert(index)) => self
                .leaf(path)
                .is_ok_and(|leaf| index.0 <= leaf.tabs.len()),
            TabDestination::Node(path, TabInsert::Append) => self.leaf(path).is_ok(),
            TabDestination::EmptySurface(surface) => self
                .get_surface(surface)
                .is_some_and(Surface::is_empty),
            TabDestination::Window(rect) => {
                rect.min.x.is_finite()
                    && rect.min.y.is_finite()
                    && rect.max.x.is_finite()
                    && rect.max.y.is_finite()
                    && rect.width() > 0.0
                    && rect.height() > 0.0
            },
        }
    }

    /// Moves a whole leaf, all its tabs in their order, as a tab is moved: into another leaf's
    /// tabs ([`TabInsert::Insert`] at an index, [`TabInsert::Append`] after them), beside a leaf
    /// ([`TabInsert::Split`]), into an empty surface, or into a new window. The leaf's active tab
    /// stays the active one where it lands; the leaf it leaves goes, and a window left empty too.
    ///
    /// Nothing happens when `src` is not a leaf holding tabs, or when `dst` is that leaf itself.
    ///
    /// ```
    /// # use egui_stack::{DockState, NodeIndex, NodePath, SurfaceIndex, TabDestination, TabIndex, TabInsert};
    /// let mut dock = DockState::new(vec!["a", "b"]);
    /// let [left, right] = dock.main_surface_mut().split_right(NodeIndex::root(), 0.5, vec!["c", "d", "e"]);
    /// let [left, right] = [left, right].map(|n| NodePath::new(SurfaceIndex::main(), n));
    /// dock.leaf_mut(right).unwrap().active = TabIndex(1); // "d"
    /// dock.move_node(right, TabDestination::Node(left, TabInsert::Append));
    /// let a = dock.find_tab(&"a").unwrap().node_path();
    /// let leaf = dock.leaf(a).unwrap();
    /// assert_eq!(leaf.tabs, ["a", "b", "c", "d", "e"]);
    /// assert_eq!(leaf.active, TabIndex(3));
    /// ```
    pub fn move_node(&mut self, src: NodePath, dst: impl Into<TabDestination>) -> bool {
        let dst = dst.into();
        // Validate both endpoints before removing anything from the source.
        if !self.valid_move_destination(&dst) {
            return false;
        }
        if matches!(dst, TabDestination::Node(d, _) if d == src) {
            return false;
        }
        let Ok(leaf) = self.leaf(src) else {
            return false;
        };
        if leaf.tabs.is_empty() {
            return false;
        }
        // valid_move_destination already verifies the destination leaf.
        // No tree mutation occurs between validation and the move.
        let leaf = self.leaf_mut(src).expect("validated source leaf");
        let active = leaf.active.0.min(leaf.tabs.len() - 1);
        let tabs = std::mem::take(&mut leaf.tabs);
        // Splitting the root relocates its old contents into a child. If
        // that root contains the emptied source, cleanup must follow it.
        let mut cleanup_source = src;
        match dst {
            TabDestination::Window(rect) => {
                let surface = self.add_window(tabs);
                if let Ok(leaf) = self.leaf_mut(NodePath::new(surface, NodeIndex::root())) {
                    leaf.active = TabIndex(active);
                }
                let from_dock = !self.is_window(src.surface);
                let state = self.get_window_state_mut(surface).unwrap();
                state.set_position(rect.min);
                if from_dock {
                    state.set_size(rect.size() * 0.8);
                } else {
                    state.set_size(rect.size());
                }
            }
            TabDestination::Node(d, insert) => match insert {
                TabInsert::Split(split) => {
                    let mut node = Node::leaf_with(tabs);
                    if let Some(leaf) = node.get_leaf_mut() {
                        leaf.active = TabIndex(active);
                    }
                    let [old_node, _new_node] =
                        self[d.surface].split(d.node, split, 0.5, node);
                    if d == cleanup_source {
                        cleanup_source.node = old_node;
                    }
                }
                TabInsert::Insert(_) | TabInsert::Append => {
                    let leaf = self.leaf_mut(d).expect("validated destination leaf");
                    let at = match insert {
                        TabInsert::Insert(i) => i.0.min(leaf.tabs.len()),
                        _ => leaf.tabs.len(),
                    };
                    leaf.tabs.splice(at..at, tabs);
                    leaf.active = TabIndex(at + active);
                }
            },
            TabDestination::EmptySurface(surface) => {
                let mut tree = Tree::new(tabs);
                if let Some(leaf) = tree.root_node_mut().and_then(Node::get_leaf_mut) {
                    leaf.active = TabIndex(active);
                }
                // An empty slot has no tree and cannot be accessed through IndexMut.
                // Destination validity was checked before the source was detached.
                self.invalidate_surface_identity(surface);
                self.surfaces[surface.0] = SurfaceSlot::new(Surface::Main(tree));
            }
        }
        self.cleanup_moved_source(cleanup_source);
        true
    }

    /// Remove a source leaf emptied by a completed move, then close its empty
    /// floating surface. Called only after the destination has received the tabs.
    fn cleanup_moved_source(&mut self, source: NodePath) {
        if self.leaf(source).is_ok_and(|leaf| leaf.tabs.is_empty()) {
            // The checked public operation also removes a floating surface
            // when its final leaf disappears.
            self.remove_leaf(source);
        }
    }

    /// Whether a valid tab move would leave the source leaf unchanged.
    /// Insertion indices are measured before removing the source tab.
    pub(crate) fn tab_move_is_noop(&self, src: TabPath, destination: &TabDestination) -> bool {
        let TabDestination::Node(target, insert) = destination else {
            return false;
        };
        if src.node_path() != *target {
            return false;
        }
        let Ok(leaf) = self.leaf(src.node_path()) else {
            return false;
        };
        let count = leaf.tabs.len();
        if src.tab.0 >= count {
            return false;
        }
        match insert {
            TabInsert::Insert(index) => {
                index.0 == src.tab.0 || index.0 == src.tab.0 + 1
            }
            TabInsert::Append => src.tab.0 + 1 == count,
            TabInsert::Split(_) => count == 1,
        }
    }

    /// Moves a tab to another leaf, a split, an empty surface, or a window.
    /// Returns false without changing the tree when the move is invalid or a no-op.
    pub fn move_tab(&mut self, src: TabPath, dst_tab: impl Into<TabDestination>) -> bool {
        let destination = dst_tab.into();
        if self.leaf(src.node_path()).is_err_or(|leaf| src.tab.0 >= leaf.tabs.len()) {
            return false;
        }
        if !self.valid_move_destination(&destination) || self.tab_move_is_noop(src, &destination) {
            return false;
        }
        // Splitting the root relocates its old leaf to a newly allocated node.
        // Track that relocation so cleanup never targets the new container.
        let mut cleanup_source = src.node_path();
        match destination {
            TabDestination::Window(position) => {
                return self.detach_tab(src, position).is_some();
            }
            TabDestination::Node(dst, dst_tab) => {
                // Call `Node::remove_tab` to avoid auto remove of the node by `Tree::remove_tab` from Tree.
                let tab = self[src.node_path()].remove_tab(src.tab).unwrap();
                match dst_tab {
                    TabInsert::Split(split) => {
                        let [old_node, _new_node] =
                            self[dst.surface].split(dst.node, split, 0.5, Node::leaf(tab));
                        if dst == cleanup_source {
                            cleanup_source.node = old_node;
                        }
                    }
                    TabInsert::Insert(index) => {
                        // Insertion indices describe the destination before removal.
                        // Removing an earlier tab shifts later slots left by one.
                        let insertion = if src.node_path() == dst && src.tab.0 < index.0 {
                            index.0 - 1
                        } else {
                            index.0
                        };
                        self[dst.surface][dst.node].insert_tab(TabIndex(insertion), tab);
                    }
                    TabInsert::Append => self[dst.surface][dst.node].append_tab(tab),
                }
            }
            TabDestination::EmptySurface(dst_surface) => {
                let tab = self[src.node_path()].remove_tab(src.tab).unwrap();
                self.invalidate_surface_identity(dst_surface);
                self.surfaces[dst_surface.0] = SurfaceSlot::new(Surface::Main(Tree::new(vec![tab])));
            }
        }
        self.cleanup_moved_source(cleanup_source);
        true
    }

    /// Closes the tab at `path` as the dock's close buttons do: asks
    /// [`TabViewer::on_close`], then removes the tab on [`OnCloseResponse::Close`], makes it the
    /// active tab of the focused leaf on [`OnCloseResponse::Focus`], and leaves it on
    /// [`OnCloseResponse::Ignore`]. A tab that is not [closeable](TabViewer::is_closeable) is left
    /// alone, and `Ignore` returned.
    ///
    /// Returns the viewer's answer. Use [`remove_tab`](Self::remove_tab) to remove a tab without
    /// asking.
    ///
    /// # Errors
    ///
    /// Returns `Err` if there is no tab at `path`.
    pub fn close_tab(
        &mut self,
        path: TabPath,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) -> Result<OnCloseResponse> {
        // Reject invalid paths before granting mutable access to the slot.
        let tab = self.leaf(path.node_path())?
            .tabs
            .get(path.tab.0)
            .ok_or(Error::InvalidTab)?;
        // is_closeable only borrows the tab immutably. A veto must not
        // invalidate the destination identity or grant mutable access.
        if !tab_viewer.is_closeable(tab) {
            return Ok(OnCloseResponse::Ignore);
        }
        let leaf = self.leaf_mut(path.node_path())?;
        let response = tab_viewer.on_close(&mut leaf.tabs[path.tab.0]);
        match response {
            OnCloseResponse::Close => {
                self.remove_tab(path);
            }
            OnCloseResponse::Focus => self.focus_tab(path)?,
            OnCloseResponse::Ignore => (),
        }
        Ok(response)
    }

    /// Makes the tab at `path` the active tab of its leaf, and that leaf the focused one, as
    /// clicking the tab does.
    ///
    /// It does not expand the leaf if it is collapsed: focusing and expanding stay apart;
    /// [`set_collapsed`](Self::set_collapsed) expands it.
    ///
    /// # Errors
    ///
    /// Returns `Err` if there is no tab at `path`.
    pub fn focus_tab(&mut self, path: TabPath) -> Result {
        self.set_active_tab(path)?;
        self.set_focused_node_and_surface(path.node_path());
        Ok(())
    }

    /// Collapses (or expands) the tab group at `path` as its collapse button does: the group
    /// shows only its tab bar, and its tree is updated, so that a float whose groups are all
    /// collapsed shrinks to its tab bars, and grows back to the height it had once one is
    /// expanded. See [`Tree::set_leaf_collapsed`].
    ///
    /// [`Node::set_collapsed`](crate::Node::set_collapsed) sets the node's flag only, and
    /// leaves its tree's state stale.
    ///
    /// # Errors
    ///
    /// Returns `Err` if there is no node at `path`, or if it is not a leaf.
    pub fn set_collapsed(&mut self, path: NodePath, collapsed: bool) -> Result {
        self.leaf(path)?;
        let tree = &mut self[path.surface];
        tree.set_leaf_collapsed(path.node, collapsed);
        if !collapsed {
            if let Some(window) = self.get_window_state_mut(path.surface) {
                window.set_new(true);
            }
        } else if let Some(height) = tree
            .root_node()
            .filter(|root| root.is_collapsed())
            .map(|root| root.rect().map_or(0.0, |rect| rect.height()))
            && let Some(window) = self.get_window_state_mut(path.surface)
        {
            window.set_expanded_height(height);
        }
        Ok(())
    }

    /// Puts the tab at `path` in a new window whose corner is at `position`, as the tab menu's
    /// Eject does: the window takes the size of the tab's leaf, or 100 by 150 points if the leaf
    /// has not been shown yet. See [`detach_tab`](Self::detach_tab) to choose the window's rect.
    ///
    /// Returns the new window's surface index, or `None` for an invalid tab path.
    pub fn eject_tab(&mut self, path: TabPath, position: Pos2) -> Option<SurfaceIndex> {
        self.leaf(path.node_path())
            .ok()?
            .tabs
            .get(path.tab.0)?;
        let size = self[path.node_path()]
            .rect()
            .map_or(Vec2::new(100., 150.), |rect| rect.size());
        self.detach_tab(path, Rect::from_min_size(position, size))
    }

    /// Takes a tab out of its current surface and puts it in a new window.
    /// Returns the new window's surface index, or `None` if the source tab is invalid.
    pub fn detach_tab(&mut self, src: TabPath, window_rect: Rect) -> Option<SurfaceIndex> {
        if !self.valid_move_destination(&TabDestination::Window(window_rect)) {
            return None;
        }
        self.leaf(src.node_path())
            .ok()?
            .tabs
            .get(src.tab.0)?;
        // Keep the source leaf in place until the destination has received the tab.
        let tab = self[src.node_path()].remove_tab(src.tab)?;
        let surface_index = self.add_window(vec![tab]);

        // Set the window size and position to match `window_rect`.
        let from_dock = !self.is_window(src.surface);
        let state = self.get_window_state_mut(surface_index).unwrap();
        state.set_position(window_rect.min);
        if from_dock {
            state.set_size(window_rect.size() * 0.8);
        } else {
            state.set_size(window_rect.size());
        }

        // Clean up any empty leaves and surfaces which may be left behind from the detachment.
        self.cleanup_moved_source(src.node_path());
        Some(surface_index)
    }

    /// Returns the currently focused leaf if there is one.
    #[inline]
    pub fn focused_leaf(&self) -> Option<NodePath> {
        let surface = self.focused_surface?;
        self[surface].focused_leaf().map(|leaf| NodePath {
            surface,
            node: leaf,
        })
    }

    /// Removes a tab at the specified `path`.
    /// This method will yield the removed tab, or `None` if it doesn't exist.
    pub fn remove_tab(&mut self, path: TabPath) -> Option<Tab> {
        // A rejected removal must not mutate the tree or close an empty window.
        self.leaf(path.node_path())
            .ok()
            .filter(|leaf| path.tab.0 < leaf.tabs.len())?;
        let removed_tab = self[path.surface].remove_tab((path.node, path.tab))?;
        if self.is_window(path.surface) && self[path.surface].is_empty() {
            self.remove_surface(path.surface);
        }
        Some(removed_tab)
    }

    /// Removes a leaf at `path`, returning whether a leaf was removed.
    /// Invalid paths and non-leaf nodes leave the tree unchanged.
    pub fn remove_leaf(&mut self, path: NodePath) -> bool {
        if self.leaf(path).is_err() {
            return false;
        }
        self[path.surface].remove_leaf(path.node);
        if self.is_window(path.surface) && self[path.surface].is_empty() {
            self.remove_surface(path.surface);
        }
        true
    }

    /// Creates two new nodes by splitting a given `parent` node and assigns them as its children. The first (old) node
    /// inherits content of the `parent` from before the split, and the second (new) has `tabs`.
    ///
    /// `fraction` (in range 0..=1) specifies how much of the `parent` node's area the old node will occupy after the
    /// split.
    ///
    /// The new node is placed relatively to the old node, in the direction specified by `split`.
    ///
    /// Returns the indices of the old node and the new node.
    pub fn split(
        &mut self,
        parent_path: NodePath,
        split: Split,
        fraction: f32,
        new: Node<Tab>,
    ) -> [NodeIndex; 2] {
        let index = self[parent_path.surface].split(parent_path.node, split, fraction, new);
        self.focused_surface = Some(parent_path.surface);
        index
    }

    /// Adds a window with its own list of tabs.
    ///
    /// Returns the [`SurfaceIndex`] of the new window, which will remain constant through the windows lifetime.
    ///
    /// This is the legacy floating window, an `egui::Window` drawn by the
    /// [`DockArea`](crate::DockArea) of a dock that is not in a [`DockSpace`](crate::DockSpace):
    /// it has no move handle and none of the floats' gestures. Prefer floats: put the dock in a
    /// [`DockSpace`](crate::DockSpace), draw its floats with [`FloatsArea`](crate::FloatsArea),
    /// and open one with [`DockSpace::float_out`](crate::DockSpace::float_out).
    pub fn add_window(&mut self, tabs: Vec<Tab>) -> SurfaceIndex {
        let surface = Surface::Window(Tree::new(tabs), WindowState::new());
        let index = self.find_empty_surface_index();
        self.invalidate_surface_identity(index);
        if index.0 < self.surfaces.len() {
            self.surfaces[index.0] = SurfaceSlot::new(surface);
        } else {
            self.surfaces.push(SurfaceSlot::new(surface));
        }
        index
    }

    /// Finds the first empty surface index which may be used.
    ///
    /// **WARNING**: in cases where one isn't found, `SurfaceIndex(self.surfaces.len())` is used.
    /// therefore it's not inherently safe to index the [`DockState`] with this index, as it may panic.
    fn find_empty_surface_index(&self) -> SurfaceIndex {
        // Find the first possible empty surface to insert our window into.
        // Starts at 1 as 0 is always the main surface.
        let empty = (self.surfaces.iter().enumerate().skip(1)).find(|(_, s)| s.is_empty());
        SurfaceIndex(empty.map_or(self.surfaces.len(), |(i, _)| i))
    }

    /// Ensures that the surface at `index` contains a [`Tree`]
    ///
    /// If the surface is [`Empty`](Surface::Empty), builds a [`Surface::Main`]
    /// for the main surface or a [`Surface::Window`] for other surfaces.
    ///
    /// # Panics
    /// If `index` is not a valid `SurfaceIndex`
    fn ensure_tree(&mut self, index: SurfaceIndex) {
        if self.surfaces[index.0].is_empty() {
            self.invalidate_surface_identity(index);
            self.surfaces[index.0] = SurfaceSlot::new(if index == SurfaceIndex::main() {
                Surface::Main(Tree::new(vec![]))
            } else {
                Surface::Window(Tree::new(vec![]), WindowState::default())
            });
        }
    }

    /// Pushes `tab` to the currently focused leaf.
    ///
    /// If no leaf is focused it will be pushed to the first available leaf.
    ///
    /// If no leaf is available then a new leaf will be created.
    pub fn push_to_focused_leaf(&mut self, tab: Tab) {
        let surface_index = self.focused_surface.unwrap_or(SurfaceIndex::main());
        self.ensure_tree(surface_index);
        self[surface_index].push_to_focused_leaf(tab)
    }

    /// Push a tab to the first available `Leaf` or create a new leaf if an `Empty` node is encountered.
    pub fn push_to_first_leaf(&mut self, tab: Tab) {
        self.ensure_tree(SurfaceIndex::main());
        self[SurfaceIndex::main()].push_to_first_leaf(tab);
    }

    /// Returns the current number of surfaces.
    pub fn surfaces_count(&self) -> usize {
        self.surfaces.len()
    }

    /// Returns an [`Iterator`] over all surfaces.
    pub fn iter_surfaces(&self) -> impl Iterator<Item = &Surface<Tab>> {
        self.surfaces.iter().map(|slot| &slot.surface)
    }

    /// Returns an [`Iterator`] over all surfaces with their corresponding [`SurfaceIndex`].
    pub fn iter_surfaces_indexed(&self) -> impl Iterator<Item = (SurfaceIndex, &Surface<Tab>)> {
        self.surfaces
            .iter()
            .enumerate()
            .map(|(index, slot)| (SurfaceIndex(index), &slot.surface))
    }

    /// Returns a mutable [`Iterator`] over all surfaces.
    pub fn iter_surfaces_mut(&mut self) -> impl Iterator<Item = &mut Surface<Tab>> {
        for slot in &mut self.surfaces {
            slot.identity = 0;
        }
        self.surfaces.iter_mut().map(|slot| &mut slot.surface)
    }

    /// Returns a mutable [`Iterator`] over all surfaces with their corresponding [`SurfaceIndex`].
    pub fn iter_surfaces_mut_indexed(
        &mut self,
    ) -> impl Iterator<Item = (SurfaceIndex, &mut Surface<Tab>)> {
        for slot in &mut self.surfaces {
            slot.identity = 0;
        }
        self.surfaces
            .iter_mut()
            .enumerate()
            .map(|(index, slot)| (SurfaceIndex(index), &mut slot.surface))
    }

    /// Returns an [`Iterator`] of **all** underlying nodes in the dock state,
    /// and the indices of containing surfaces.
    pub fn iter_all_nodes(&self) -> impl Iterator<Item = (NodePath, &Node<Tab>)> {
        self.iter_surfaces_indexed()
            .flat_map(|(surface_index, surface)| {
                surface.iter_nodes_indexed().map(move |(node_index, node)| {
                    (
                        NodePath {
                            surface: surface_index,
                            node: node_index,
                        },
                        node,
                    )
                })
            })
    }

    /// Returns a mutable [`Iterator`] of **all** underlying nodes in the dock state,
    /// and the indices of containing surfaces.
    pub fn iter_all_nodes_mut(&mut self) -> impl Iterator<Item = (NodePath, &mut Node<Tab>)> {
        self.iter_surfaces_mut_indexed()
            .flat_map(|(surface_index, surface)| {
                surface
                    .iter_nodes_mut_indexed()
                    .map(move |(node_index, node)| {
                        (
                            NodePath {
                                surface: surface_index,
                                node: node_index,
                            },
                            node,
                        )
                    })
            })
    }

    /// Returns an [`Iterator`] of **all** tabs in the dock state,
    /// and the indices of containing surfaces and nodes.
    pub fn iter_all_tabs(&self) -> impl Iterator<Item = (TabPath, &Tab)> {
        self.iter_surfaces_indexed()
            .flat_map(|(surface_index, surface)| {
                surface
                    .iter_all_tabs()
                    .map(move |((node_index, tab_index), tab)| {
                        (TabPath::new(surface_index, node_index, tab_index), tab)
                    })
            })
    }

    /// Returns a mutable [`Iterator`] of **all** tabs in the dock state,
    /// and the indices of containing surfaces and nodes.
    pub fn iter_all_tabs_mut(&mut self) -> impl Iterator<Item = (TabPath, &mut Tab)> {
        self.iter_surfaces_mut_indexed()
            .flat_map(|(surface_index, surface)| {
                surface
                    .iter_all_tabs_mut()
                    .map(move |((node_index, tab_index), tab)| {
                        (TabPath::new(surface_index, node_index, tab_index), tab)
                    })
            })
    }

    /// Returns an [`Iterator`] of the underlying collection of nodes on the main surface.
    #[deprecated = "Use `dock_state.main_surface().iter()` instead"]
    pub fn iter_main_surface_nodes(&self) -> impl Iterator<Item = &Node<Tab>> {
        self[SurfaceIndex::main()].iter()
    }

    /// Returns a mutable [`Iterator`] of the underlying collection of nodes on the main surface.
    #[deprecated = "Use `dock_state.main_surface_mut().iter_mut()` instead"]
    pub fn iter_main_surface_nodes_mut(&mut self) -> impl Iterator<Item = &mut Node<Tab>> {
        self[SurfaceIndex::main()].iter_mut()
    }

    /// Returns an [`Iterator`] of **all** underlying nodes in the dock state and all subsequent trees.
    #[deprecated = "Use `iter_all_nodes` instead"]
    pub fn iter_nodes(&self) -> impl Iterator<Item = &Node<Tab>> {
        self.surfaces
            .iter()
            .filter_map(|slot| slot.surface.node_tree())
            .flat_map(|nodes| nodes.iter())
    }

    /// Returns an immutable [`Iterator`] of all [``LeafNode``]s in the dock state.
    pub fn iter_leaves(&self) -> impl Iterator<Item = (NodePath, &LeafNode<Tab>)> {
        self.iter_all_nodes()
            .filter_map(|(index, node)| node.get_leaf().map(|leaf| (index, leaf)))
    }

    /// Returns a mutable [`Iterator`] of all [`LeafNode`]s in the dock state.
    pub fn iter_leaves_mut(&mut self) -> impl Iterator<Item = (NodePath, &mut LeafNode<Tab>)> {
        self.iter_all_nodes_mut()
            .filter_map(|(index, node)| node.get_leaf_mut().map(|leaf| (index, leaf)))
    }

    /// Returns a new [`DockState`] while mapping and filtering the tab type.
    /// Any remaining empty [`Node`]s are removed, and windows left empty become
    /// [`Surface::Empty`], so that every surface keeps its index.
    ///
    /// ```
    /// # use egui_stack::{DockState, Node};
    /// let dock_state = DockState::new(vec![1, 2, 3]);
    /// let mapped_dock_state = dock_state.filter_map_tabs(|tab| (tab % 2 == 1).then(|| tab.to_string()));
    ///
    /// let tabs: Vec<_> = mapped_dock_state.iter_all_tabs().map(|(_, tab)| tab.to_owned()).collect();
    /// assert_eq!(tabs, vec!["1".to_string(), "3".to_string()]);
    /// ```
    pub fn filter_map_tabs<F, NewTab>(&self, mut function: F) -> DockState<NewTab>
    where
        F: FnMut(&Tab) -> Option<NewTab>,
    {
        let DockState {
            surfaces,
            focused_surface,
            translations,
            ..
        } = self;
        // Emptied windows leave a hole, so that the surfaces after them keep their index.
        let mut surfaces: Vec<SurfaceSlot<NewTab>> = surfaces
            .iter()
            .map(|slot| SurfaceSlot::new(slot.surface.filter_map_tabs(&mut function)))
            .collect();
        while surfaces.len() > 1 && surfaces.last().is_some_and(|slot| slot.is_empty()) {
            surfaces.pop();
        }
        let focused_surface = focused_surface
            .filter(|focused| surfaces.get(focused.0).is_some_and(|s| !s.is_empty()));
        DockState {
            surfaces,
            focused_surface,
            translations: translations.clone(),
        }
    }

    /// Returns a new [`DockState`] while mapping the tab type.
    ///
    /// ```
    /// # use egui_stack::{DockState, Node};
    /// let dock_state = DockState::new(vec![1, 2, 3]);
    /// let mapped_dock_state = dock_state.map_tabs(|tab| tab.to_string());
    ///
    /// let tabs: Vec<_> = mapped_dock_state.iter_all_tabs().map(|(_, tab)| tab.to_owned()).collect();
    /// assert_eq!(tabs, vec!["1".to_string(), "2".to_string(), "3".to_string()]);
    /// ```
    pub fn map_tabs<F, NewTab>(&self, mut function: F) -> DockState<NewTab>
    where
        F: FnMut(&Tab) -> NewTab,
    {
        self.filter_map_tabs(move |tab| Some(function(tab)))
    }

    /// Returns a new [`DockState`] while filtering the tab type.
    /// Any remaining empty [`Node`]s and [`Surface`]s are removed.
    ///
    /// ```
    /// # use egui_stack::{DockState, Node};
    /// let dock_state = DockState::new(["tab1", "tab2", "outlier"].map(str::to_string).to_vec());
    /// let filtered_dock_state = dock_state.filter_tabs(|tab| tab.starts_with("tab"));
    ///
    /// let tabs: Vec<_> = filtered_dock_state.iter_all_tabs().map(|(_, tab)| tab.to_owned()).collect();
    /// assert_eq!(tabs, vec!["tab1".to_string(), "tab2".to_string()]);
    /// ```
    pub fn filter_tabs<F>(&self, mut predicate: F) -> DockState<Tab>
    where
        F: FnMut(&Tab) -> bool,
        Tab: Clone,
    {
        self.filter_map_tabs(move |tab| predicate(tab).then(|| tab.clone()))
    }

    /// Removes all tabs for which `predicate` returns `false`.
    /// Any remaining empty [`Node`]s are also removed, and windows left empty become
    /// [`Surface::Empty`], so that every surface keeps its index.
    ///
    /// ```
    /// # use egui_stack::{DockState, Node};
    /// let mut dock_state = DockState::new(["tab1", "tab2", "outlier"].map(str::to_string).to_vec());
    /// dock_state.retain_tabs(|tab| tab.starts_with("tab"));
    ///
    /// let tabs: Vec<_> = dock_state.iter_all_tabs().map(|(_, tab)| tab.to_owned()).collect();
    /// assert_eq!(tabs, vec!["tab1".to_string(), "tab2".to_string()]);
    /// ```
    pub fn retain_tabs<F>(&mut self, mut predicate: F)
    where
        F: FnMut(&mut Tab) -> bool,
    {
        // Docked surfaces keep their (maybe empty) tree; emptied windows leave a hole, so that
        // the surfaces after them keep their index.
        // Retention may change tab order, remove nodes, or replace an occupied
        // window with a vacancy. Invalidate *before* invoking user code so even
        // a retained drag report cannot outlive a reentrant structural edit.
        for slot in &mut self.surfaces {
            slot.identity = 0;
        }
        for slot in &mut self.surfaces {
            match &mut slot.surface {
                Surface::Main(tree) => tree.retain_tabs(&mut predicate),
                surface => surface.retain_tabs(&mut predicate),
            }
        }
        while self.surfaces.len() > 1 && self.surfaces.last().is_some_and(|slot| slot.is_empty()) {
            let index = self.surfaces.len() - 1;
            self.invalidate_surface_identity(SurfaceIndex(index));
            self.surfaces.pop();
        }
        if let Some(focused) = self.focused_surface
            && !self.is_surface_valid(focused)
        {
            self.focused_surface = None;
        }
    }

    /// Find a tab based on the conditions of a function.
    ///
    /// Returns the full path to that tab if it was found.
    ///
    /// The returned [`NodeIndex`] will always point to a [`Node::Leaf`].
    ///
    /// In case there are several hits, only the first is returned.
    pub fn find_tab_from(&self, predicate: impl Fn(&Tab) -> bool) -> Option<TabPath> {
        for &surface_index in self.valid_surface_indices().iter() {
            if self.surfaces[surface_index.0].is_empty() {
                continue;
            }
            if let Some((node_index, tab_index)) = self[surface_index].find_tab_from(&predicate) {
                return Some(TabPath::new(surface_index, node_index, tab_index));
            }
        }
        None
    }
}

impl<Tab> DockState<Tab>
where
    Tab: PartialEq,
{
    /// Find the given tab.
    ///
    /// Returns in which node and where in that node the tab is.
    ///
    /// The returned [`NodeIndex`] will always point to a [`Node::Leaf`].
    ///
    /// In case there are several hits, only the first is returned.
    ///
    /// See also: [`find_main_surface_tab`](DockState::find_main_surface_tab)
    pub fn find_tab(&self, needle_tab: &Tab) -> Option<TabPath> {
        self.find_tab_from(|tab| tab == needle_tab)
    }

    /// Find the given tab on the main surface.
    ///
    /// Returns which node and where in that node the tab is.
    ///
    /// The returned [`NodeIndex`] will always point to a [`Node::Leaf`].
    ///
    /// In case there are several hits, only the first is returned.
    pub fn find_main_surface_tab(&self, needle_tab: &Tab) -> Option<(NodeIndex, TabIndex)> {
        self[SurfaceIndex::main()].find_tab(needle_tab)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn removing_invalid_tab_path_preserves_tree_and_focus() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        dock.set_focused_node_and_surface(root);
        let focus = dock.focused_leaf();

        for path in [
            TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(99)),
            TabPath::new(SurfaceIndex::main(), NodeIndex(999), TabIndex(0)),
            TabPath::new(SurfaceIndex(999), NodeIndex::root(), TabIndex(0)),
        ] {
            assert_eq!(dock.remove_tab(path), None);
            assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second"]);
            assert_eq!(dock.focused_leaf(), focus);
        }
    }

    #[test]
    fn invalid_leaf_removal_preserves_tree_and_surfaces() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let before = dock.surfaces.len();
        let focus = dock.focused_leaf();
        assert!(!dock.remove_leaf(NodePath::new(
            SurfaceIndex::main(),
            NodeIndex(999),
        )));
        assert!(!dock.remove_leaf(NodePath::new(
            SurfaceIndex(999),
            NodeIndex::root(),
        )));
        assert_eq!(dock.surfaces.len(), before);
        assert_eq!(dock.focused_leaf(), focus);
        assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second"]);
        assert!(dock.remove_leaf(root));
        assert!(dock.get_surface(SurfaceIndex::main()).is_some());
    }

    #[test]
    fn removing_non_leaf_is_rejected_and_removing_last_window_leaf_closes_surface() {
        let mut dock = DockState::new(vec!["first"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        assert!(!dock.remove_leaf(root));
        assert!(dock.find_tab(&"first").is_some());
        assert!(dock.find_tab(&"second").is_some());

        let window = dock.add_window(vec!["floating"]);
        let floating = NodePath::new(window, NodeIndex::root());
        assert!(dock.remove_leaf(floating));
        assert!(dock.get_surface(window).is_none_or(Surface::is_empty));
        assert!(dock.find_tab(&"first").is_some());
        assert!(dock.find_tab(&"second").is_some());
    }

    #[test]
    fn invalid_detach_and_eject_are_noops() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let before = dock.surfaces.len();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 240.0));
        for path in [
            TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(99)),
            TabPath::new(SurfaceIndex::main(), NodeIndex(999), TabIndex(0)),
            TabPath::new(SurfaceIndex(999), NodeIndex::root(), TabIndex(0)),
        ] {
            assert_eq!(dock.detach_tab(path, rect), None);
            assert_eq!(dock.eject_tab(path, Pos2::ZERO), None);
        }
        assert_eq!(dock.surfaces.len(), before);
        assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second"]);
    }

    #[test]
    fn invalid_move_sources_do_not_create_windows_or_change_tabs() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let surfaces_before = dock.surfaces.len();
        let window = TabDestination::Window(Rect::from_min_size(
            Pos2::ZERO,
            Vec2::new(320.0, 240.0),
        ));
        for path in [
            TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(99)),
            TabPath::new(SurfaceIndex::main(), NodeIndex(999), TabIndex(0)),
            TabPath::new(SurfaceIndex(999), NodeIndex::root(), TabIndex(0)),
        ] {
            assert!(!dock.move_tab(path, window));
        }
        for path in [
            NodePath::new(SurfaceIndex::main(), NodeIndex(999)),
            NodePath::new(SurfaceIndex(999), NodeIndex::root()),
        ] {
            assert!(!dock.move_node(path, window));
        }
        assert_eq!(dock.surfaces.len(), surfaces_before);
        assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second"]);
        assert!(dock.find_tab(&"first").is_some());
        assert!(dock.find_tab(&"second").is_some());
    }

    #[test]
    fn splitting_an_empty_node_is_rejected_without_removing_source() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let empty_surface = dock.add_window(vec![]);
        let empty = NodePath::new(empty_surface, NodeIndex::root());
        let source = TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0));
        assert!(!dock.move_tab(
            source,
            TabDestination::Node(empty, TabInsert::Split(crate::Split::Right)),
        ));
        assert!(!dock.move_node(
            root,
            TabDestination::Node(empty, TabInsert::Split(crate::Split::Right)),
        ));
        assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second"]);
        assert!(dock.get_surface(empty_surface).unwrap().is_empty());
    }

    #[test]
    fn moving_tab_into_vacant_slot_creates_a_tree_and_invalidates_slot_identity() {
        let mut dock = DockState::new(vec!["a", "b"]);
        let vacant = dock.add_window(vec!["temporary"]);
        let trailing = dock.add_window(vec!["trailing"]);
        dock.remove_surface(vacant).unwrap();
        assert!(dock.is_surface_valid(trailing));
        let previous = dock.capture_surface_identity(vacant).unwrap();

        assert!(dock.move_tab(
            TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0)),
            TabDestination::EmptySurface(vacant),
        ));
        assert!(matches!(dock.get_surface(vacant), Some(Surface::Main(_))));
        assert_eq!(dock.current_surface_identity(vacant), None);
        assert_ne!(dock.capture_surface_identity(vacant), Some(previous));
        assert_eq!(dock.find_tab(&"a").unwrap().surface, vacant);
    }

    #[test]
    fn moving_leaf_into_vacant_slot_preserves_tabs_and_active_selection() {
        let mut dock = DockState::new(vec!["a", "b"]);
        let vacant = dock.add_window(vec!["temporary"]);
        let trailing = dock.add_window(vec!["trailing"]);
        dock.remove_surface(vacant).unwrap();
        assert!(dock.is_surface_valid(trailing));
        let previous = dock.capture_surface_identity(vacant).unwrap();

        assert!(dock.move_node(
            NodePath::new(SurfaceIndex::main(), NodeIndex::root()),
            TabDestination::EmptySurface(vacant),
        ));
        assert!(matches!(dock.get_surface(vacant), Some(Surface::Main(_))));
        assert_eq!(dock.current_surface_identity(vacant), None);
        assert_ne!(dock.capture_surface_identity(vacant), Some(previous));
        assert_eq!(dock.find_tab(&"a").unwrap().surface, vacant);
        assert_eq!(dock.find_tab(&"b").unwrap().surface, vacant);
    }

    #[test]
    fn retaining_tabs_invalidates_identity_even_when_surface_stays_occupied() {
        let mut dock = DockState::new(vec!["keep", "remove"]);
        let other = dock.add_window(vec!["other"]);
        let main = SurfaceIndex::main();
        let main_before = dock.capture_surface_identity(main).unwrap();
        let other_before = dock.capture_surface_identity(other).unwrap();

        dock.retain_tabs(|tab| *tab != "remove");

        assert_eq!(dock.current_surface_identity(main), None);
        assert_eq!(dock.current_surface_identity(other), None);
        assert_ne!(dock.capture_surface_identity(main), Some(main_before));
        assert_ne!(dock.capture_surface_identity(other), Some(other_before));
    }

    #[test]
    fn mapping_tabs_creates_a_distinct_surface_identity_namespace() {
        let mut original = DockState::new(vec![1, 2]);
        let extra = original.add_window(vec![3]);
        let main = SurfaceIndex::main();
        let old_main = original.capture_surface_identity(main).unwrap();
        let old_extra = original.capture_surface_identity(extra).unwrap();

        let mut mapped = original.map_tabs(|tab| tab.to_string());
        assert_eq!(mapped.current_surface_identity(main), None);
        assert_eq!(mapped.current_surface_identity(extra), None);
        assert_ne!(mapped.capture_surface_identity(main), Some(old_main));
        assert_ne!(mapped.capture_surface_identity(extra), Some(old_extra));
        assert_eq!(original.current_surface_identity(main), Some(old_main));
        assert_eq!(original.current_surface_identity(extra), Some(old_extra));
    }

    #[test]
    fn cloning_dock_state_does_not_copy_runtime_surface_identity() {
        let mut original = DockState::new(vec!["main"]);
        let extra = original.add_window(vec!["other"]);
        let main_id = original.capture_surface_identity(SurfaceIndex::main()).unwrap();
        let extra_id = original.capture_surface_identity(extra).unwrap();

        let mut cloned = original.clone();
        assert_eq!(cloned.current_surface_identity(SurfaceIndex::main()), None);
        assert_eq!(cloned.current_surface_identity(extra), None);
        assert_ne!(cloned.capture_surface_identity(SurfaceIndex::main()), Some(main_id));
        assert_ne!(cloned.capture_surface_identity(extra), Some(extra_id));
        assert_eq!(original.current_surface_identity(SurfaceIndex::main()), Some(main_id));
        assert_eq!(original.current_surface_identity(extra), Some(extra_id));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn serialized_surface_slots_exclude_runtime_identity() {
        let mut dock = DockState::new(vec!["main"]);
        let other = dock.add_window(vec!["other"]);
        let main = SurfaceIndex::main();
        let original_main = dock.capture_surface_identity(main).unwrap();
        let original_other = dock.capture_surface_identity(other).unwrap();

        let json = serde_json::to_string(&dock).unwrap();
        assert!(!json.contains("identity"));
        let mut restored: DockState<&str> = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.current_surface_identity(main), None);
        assert_eq!(restored.current_surface_identity(other), None);
        assert_ne!(restored.capture_surface_identity(main), Some(original_main));
        assert_ne!(restored.capture_surface_identity(other), Some(original_other));
        assert!(restored.find_tab(&"main").is_some());
        assert!(restored.find_tab(&"other").is_some());
    }

    #[test]
    fn invalid_active_tab_does_not_invalidate_surface_identity() {
        let mut dock = DockState::new(vec!["a", "b"]);
        let main = SurfaceIndex::main();
        let identity = dock.capture_surface_identity(main).unwrap();
        let invalid = TabPath::new(main, NodeIndex::root(), TabIndex(99));

        assert!(dock.set_active_tab(invalid).is_err());
        assert!(dock.focus_tab(invalid).is_err());
        assert_eq!(dock.current_surface_identity(main), Some(identity));
        assert_eq!(dock.find_tab(&"a").unwrap().surface, main);
    }

    #[test]
    fn rejected_mutable_node_lookup_preserves_surface_identity() {
        let mut dock = DockState::new(vec!["main"]);
        let main = SurfaceIndex::main();
        let identity = dock.capture_surface_identity(main).unwrap();
        let missing = NodePath::new(main, NodeIndex(999));

        assert!(dock.node_mut(missing).is_err());
        assert!(dock.leaf_mut(missing).is_err());
        assert_eq!(dock.current_surface_identity(main), Some(identity));
    }

    #[test]
    fn rejected_move_preserves_surface_slot_identities() {
        let mut dock = DockState::new(vec!["a", "b"]);
        let other = dock.add_window(vec!["c"]);
        let main = SurfaceIndex::main();
        let main_id = dock.capture_surface_identity(main).unwrap();
        let other_id = dock.capture_surface_identity(other).unwrap();

        assert!(!dock.move_tab(
            TabPath::new(main, NodeIndex::root(), TabIndex(99)),
            TabDestination::Node(
                NodePath::new(other, NodeIndex::root()),
                TabInsert::Append,
            ),
        ));
        assert_eq!(dock.current_surface_identity(main), Some(main_id));
        assert_eq!(dock.current_surface_identity(other), Some(other_id));
        assert_eq!(dock.find_tab(&"a").unwrap().surface, main);
    }

    #[test]
    fn moving_group_to_split_preserves_destination_and_removes_source_leaf() {
        let mut dock = DockState::new(vec!["a", "b"]);
        let [left, right] = dock
            .main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["c", "d"]);
        let main = SurfaceIndex::main();
        let source = NodePath::new(main, right);
        let target = NodePath::new(main, left);

        assert!(dock.move_node(source, TabDestination::Node(
            target,
            TabInsert::Split(Split::Right),
        )));
        assert_eq!(dock.iter_all_tabs().count(), 4);
        assert!(dock.find_tab(&"a").is_some());
        assert!(dock.find_tab(&"b").is_some());
        assert!(dock.find_tab(&"c").is_some());
        assert!(dock.find_tab(&"d").is_some());
    }

    #[test]
    fn successful_cross_surface_move_invalidates_both_slots() {
        let mut dock = DockState::new(vec!["a", "b"]);
        let other = dock.add_window(vec!["c"]);
        let main = SurfaceIndex::main();
        let main_id = dock.capture_surface_identity(main).unwrap();
        let other_id = dock.capture_surface_identity(other).unwrap();

        assert!(dock.move_tab(
            TabPath::new(main, NodeIndex::root(), TabIndex(0)),
            TabDestination::Node(
                NodePath::new(other, NodeIndex::root()),
                TabInsert::Append,
            ),
        ));
        assert_eq!(dock.current_surface_identity(main), None);
        assert_eq!(dock.current_surface_identity(other), None);
        assert_ne!(dock.capture_surface_identity(main), Some(main_id));
        assert_ne!(dock.capture_surface_identity(other), Some(other_id));
        assert_eq!(dock.find_tab(&"a").unwrap().surface, other);
    }

    #[test]
    fn indexed_tree_mutation_invalidates_only_its_surface_slot() {
        let mut dock = DockState::new(vec!["main"]);
        let extra = dock.add_window(vec!["other"]);
        let main = SurfaceIndex::main();
        let main_identity = dock.capture_surface_identity(main).unwrap();
        let extra_identity = dock.capture_surface_identity(extra).unwrap();

        dock[main].push_to_first_leaf("new");

        assert_eq!(dock.current_surface_identity(main), None);
        assert_eq!(dock.current_surface_identity(extra), Some(extra_identity));
        assert_ne!(dock.capture_surface_identity(main), Some(main_identity));
    }

    #[test]
    fn indexed_node_mutation_invalidates_only_its_surface_slot() {
        let mut dock = DockState::new(vec!["main"]);
        let extra = dock.add_window(vec!["other"]);
        let main = SurfaceIndex::main();
        let main_identity = dock.capture_surface_identity(main).unwrap();
        let extra_identity = dock.capture_surface_identity(extra).unwrap();

        dock[NodePath::new(main, NodeIndex::root())].append_tab("new");

        assert_eq!(dock.current_surface_identity(main), None);
        assert_eq!(dock.current_surface_identity(extra), Some(extra_identity));
        assert_ne!(dock.capture_surface_identity(main), Some(main_identity));
    }

    #[test]
    fn direct_mutable_surface_access_invalidates_retained_identity() {
        let mut dock = DockState::new(vec!["main"]);
        let extra = dock.add_window(vec!["other"]);
        let original = dock.capture_surface_identity(extra).unwrap();
        assert!(dock.get_surface_mut(extra).is_some());
        assert_eq!(dock.current_surface_identity(extra), None);
        assert_ne!(dock.capture_surface_identity(extra), Some(original));

        let original = dock.capture_surface_identity(extra).unwrap();
        let _ = dock.iter_surfaces_mut().count();
        assert_eq!(dock.current_surface_identity(extra), None);
        assert_ne!(dock.capture_surface_identity(extra), Some(original));

        let original = dock.capture_surface_identity(extra).unwrap();
        let _ = dock.iter_surfaces_mut_indexed().count();
        assert_eq!(dock.current_surface_identity(extra), None);
        assert_ne!(dock.capture_surface_identity(extra), Some(original));
    }

    #[test]
    fn vacant_surface_identity_is_invalidated_by_reuse() {
        let mut dock = DockState::new(vec!["main"]);
        let empty = dock.add_window(vec!["temporary"]);
        let trailing = dock.add_window(vec!["trailing"]);
        dock.remove_surface(empty).unwrap();
        assert!(dock.is_surface_valid(trailing));

        let vacant_identity = dock.capture_surface_identity(empty).unwrap();
        let replacement = dock.add_docked_surface(Tree::new(vec!["new"]));
        assert_eq!(replacement, empty);
        assert_eq!(dock.current_surface_identity(empty), None);
        assert_ne!(dock.capture_surface_identity(empty), Some(vacant_identity));
    }

    #[test]
    fn recycled_surface_slot_gets_a_new_runtime_identity() {
        let mut dock = DockState::new(vec!["main"]);
        let first = dock.add_window(vec!["first"]);
        let original = dock.capture_surface_identity(first).unwrap();
        assert_eq!(dock.current_surface_identity(first), Some(original));

        assert!(dock.remove_surface(first).is_some());
        assert_eq!(dock.current_surface_identity(first), None);
        let recycled = dock.add_window(vec!["second"]);
        assert_eq!(first, recycled);
        assert_eq!(dock.current_surface_identity(recycled), None);
        let replacement = dock.capture_surface_identity(recycled).unwrap();
        assert_ne!(original, replacement);
        assert_eq!(dock.current_surface_identity(recycled), Some(replacement));
    }

    #[test]
    fn removing_surface_rejects_main_vacant_and_out_of_range_slots() {
        let mut dock = DockState::new(vec!["main"]);
        let first = dock.add_window(vec!["first"]);
        let second = dock.add_window(vec!["second"]);
        assert_eq!(dock.remove_surface(SurfaceIndex::main()).is_none(), true);
        assert!(dock.remove_surface(first).is_some());
        assert!(dock.remove_surface(first).is_none());
        assert_eq!(dock[second].num_tabs(), 1);
        assert!(dock.remove_surface(SurfaceIndex(usize::MAX)).is_none());
        assert!(dock.remove_surface(second).is_some());
        assert!(dock.remove_surface(second).is_none());
        assert_eq!(dock[SurfaceIndex::main()].num_tabs(), 1);
    }

    #[test]
    fn invalid_window_rectangles_reject_moves_without_changing_the_tree() {
        let invalid = [
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(0.0, 100.0)),
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 0.0)),
            Rect::from_min_max(egui::pos2(f32::NAN, 0.0), egui::pos2(100.0, 100.0)),
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(f32::INFINITY, 100.0)),
        ];
        for rect in invalid {
            let mut dock = DockState::new(vec!["first", "second"]);
            let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
            let tab = TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0));
            assert!(!dock.valid_move_destination(&TabDestination::Window(rect)));
            assert!(!dock.move_tab(tab, TabDestination::Window(rect)));
            assert!(!dock.move_node(root, TabDestination::Window(rect)));
            assert_eq!(dock.detach_tab(tab, rect), None);
            assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second"]);
            assert_eq!(dock.surfaces.len(), 1);
        }
    }

    #[test]
    fn splitting_ancestor_of_source_keeps_other_branches() {
        let mut dock = DockState::new(vec!["source"]);
        let [left, _right] = dock.main_surface_mut().split_right(
            NodeIndex::root(), 0.5, vec!["other"],
        );
        let source = TabPath::new(SurfaceIndex::main(), left, TabIndex(0));
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        assert!(dock.move_tab(
            source,
            TabDestination::Node(root, TabInsert::Split(crate::Split::Below)),
        ));
        assert!(dock.find_tab(&"source").is_some());
        assert!(dock.find_tab(&"other").is_some());
    }

    #[test]
    fn moving_floating_group_into_main_preserves_selection_and_closes_source() {
        let mut dock = DockState::new(vec!["main"]);
        let window = dock.add_window(vec!["first", "second", "third"]);
        let source = NodePath::new(window, NodeIndex::root());
        let target = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        dock.leaf_mut(source).unwrap().active = TabIndex(1);

        assert!(dock.move_node(source, TabDestination::Node(target, TabInsert::Append)));
        assert_eq!(
            dock.leaf(target).unwrap().tabs,
            ["main", "first", "second", "third"]
        );
        assert_eq!(dock.leaf(target).unwrap().active, TabIndex(2));
        assert!(dock.get_surface(window).is_none_or(Surface::is_empty));
    }

    #[test]
    fn moving_last_floating_tab_into_main_closes_source() {
        let mut dock = DockState::new(vec!["main"]);
        let window = dock.add_window(vec!["floating"]);
        let source = TabPath::new(window, NodeIndex::root(), TabIndex(0));
        let target = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        assert!(dock.move_tab(source, TabDestination::Node(target, TabInsert::Append)));
        assert_eq!(dock.leaf(target).unwrap().tabs, ["main", "floating"]);
        assert!(dock.get_surface(window).is_none_or(Surface::is_empty));
    }

    #[test]
    fn moving_group_into_split_of_ancestor_preserves_every_tab() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let [left, _right] = dock.main_surface_mut().split_right(
            NodeIndex::root(), 0.5, vec!["other"],
        );
        let source = NodePath::new(SurfaceIndex::main(), left);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        dock.leaf_mut(source).unwrap().active = TabIndex(1);

        assert!(dock.move_node(
            source,
            TabDestination::Node(root, TabInsert::Split(crate::Split::Below)),
        ));
        for name in ["first", "second", "other"] {
            assert!(dock.find_tab(&name).is_some(), "lost tab: {name}");
        }
        let moved = dock.find_tab(&"second").unwrap().node_path();
        let leaf = dock.leaf(moved).unwrap();
        assert_eq!(leaf.tabs, ["first", "second"]);
        assert_eq!(leaf.active, TabIndex(1));
    }

    #[test]
    fn splitting_another_leaf_keeps_source_cleanup_and_destination_intact() {
        let mut dock = DockState::new(vec!["source"]);
        let [left, right] = dock.main_surface_mut().split_right(
            NodeIndex::root(), 0.5, vec!["destination"],
        );
        let source = TabPath::new(SurfaceIndex::main(), left, TabIndex(0));
        let destination = NodePath::new(SurfaceIndex::main(), right);
        assert!(dock.move_tab(
            source,
            TabDestination::Node(destination, TabInsert::Split(crate::Split::Below)),
        ));
        assert!(dock.find_tab(&"source").is_some());
        assert!(dock.find_tab(&"destination").is_some());
        let destination_after_move = dock.find_tab(&"destination").unwrap().node_path();
        assert_eq!(
            dock.leaf(destination_after_move).unwrap().tabs,
            ["destination"]
        );
    }

    #[test]
    fn splitting_group_into_another_leaf_preserves_all_tabs_and_active_selection() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let [left, right] = dock.main_surface_mut().split_right(
            NodeIndex::root(), 0.5, vec!["destination"],
        );
        let source = NodePath::new(SurfaceIndex::main(), left);
        let destination = NodePath::new(SurfaceIndex::main(), right);
        dock.leaf_mut(source).unwrap().active = TabIndex(1);
        assert!(dock.move_node(
            source,
            TabDestination::Node(destination, TabInsert::Split(crate::Split::Below)),
        ));
        for name in ["first", "second", "destination"] {
            assert!(dock.find_tab(&name).is_some(), "lost tab: {name}");
        }
        let moved = dock.find_tab(&"second").unwrap().node_path();
        let leaf = dock.leaf(moved).unwrap();
        assert_eq!(leaf.tabs, ["first", "second"]);
        assert_eq!(leaf.active, TabIndex(1));
    }

    #[test]
    fn splitting_single_tab_root_removes_relocated_empty_source() {
        let mut dock = DockState::new(vec!["only"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let source = TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0));
        assert!(!dock.move_tab(
            source,
            TabDestination::Node(root, TabInsert::Split(crate::Split::Right)),
        ));
        assert_eq!(dock.leaf(root).unwrap().tabs, ["only"]);

        let mut dock = DockState::new(vec!["first", "second"]);
        assert!(dock.move_tab(
            source,
            TabDestination::Node(root, TabInsert::Split(crate::Split::Right)),
        ));
        for name in ["first", "second"] {
            assert!(dock.find_tab(&name).is_some());
        }
    }

    #[test]
    fn splitting_source_root_preserves_every_tab() {
        let mut dock = DockState::new(vec!["first", "second", "third"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        assert!(dock.move_tab(
            TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(1)),
            TabDestination::Node(root, TabInsert::Split(crate::Split::Right)),
        ));
        for name in ["first", "second", "third"] {
            assert!(dock.find_tab(&name).is_some(), "lost tab: {name}");
        }
        let moved = dock.find_tab(&"second").unwrap();
        assert_eq!(dock.leaf(moved.node_path()).unwrap().tabs, ["second"]);
    }

    #[test]
    fn rejected_moves_preserve_active_tab_and_focus() {
        let mut dock = DockState::new(vec!["first", "second", "third"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let source = TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(1));
        dock.set_active_tab(source).unwrap();
        dock.set_focused_node_and_surface(root);
        let before = dock.focused_leaf();

        // Both same-leaf insertion slots are no-ops for the source tab.
        for index in [1, 2] {
            assert!(!dock.move_tab(
                source,
                TabDestination::Node(root, TabInsert::Insert(TabIndex(index))),
            ));
            assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second", "third"]);
            assert_eq!(dock.leaf(root).unwrap().active, TabIndex(1));
            assert_eq!(dock.focused_leaf(), before);
        }

        // A rejected group move cannot consume its tabs or selection.
        assert!(!dock.move_node(
            root,
            TabDestination::Node(
                NodePath::new(SurfaceIndex::main(), NodeIndex(999)),
                TabInsert::Append,
            ),
        ));
        assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second", "third"]);
        assert_eq!(dock.leaf(root).unwrap().active, TabIndex(1));
        assert_eq!(dock.focused_leaf(), before);
    }

    #[test]
    fn invalid_move_endpoints_do_not_remove_source_tabs() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let invalid = NodePath::new(SurfaceIndex::main(), NodeIndex(999));
        let tab = TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0));

        assert!(!dock.move_node(
            root,
            TabDestination::Node(invalid, TabInsert::Append),
        ));
        assert!(!dock.move_tab(
            tab,
            TabDestination::Node(invalid, TabInsert::Append),
        ));
        assert!(!dock.move_tab(
            TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(999)),
            TabDestination::Node(root, TabInsert::Append),
        ));
        assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second"]);
    }

    #[test]
    fn same_leaf_group_move_is_a_noop() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        assert!(!dock.move_node(
            root,
            TabDestination::Node(root, TabInsert::Append),
        ));
        assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second"]);
    }

    #[test]
    fn empty_surface_move_rejects_an_occupied_surface() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let tab = TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0));
        assert!(!dock.move_node(root, TabDestination::EmptySurface(SurfaceIndex::main())));
        assert!(!dock.move_tab(tab, TabDestination::EmptySurface(SurfaceIndex::main())));
        assert_eq!(dock.leaf(root).unwrap().tabs, ["first", "second"]);
    }

    #[test]
    fn same_leaf_no_op_moves_preserve_order_and_active_tab() {
        let mut dock = DockState::new(vec!["a", "b", "c"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        dock.leaf_mut(root).unwrap().active = TabIndex(1);
        for (from, insertion) in [
            (0, TabInsert::Insert(TabIndex(0))),
            (0, TabInsert::Insert(TabIndex(1))),
            (1, TabInsert::Insert(TabIndex(1))),
            (1, TabInsert::Insert(TabIndex(2))),
            (2, TabInsert::Append),
        ] {
            assert!(!dock.move_tab(
                TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(from)),
                TabDestination::Node(root, insertion),
            ));
            let leaf = dock.leaf(root).unwrap();
            assert_eq!(leaf.tabs, ["a", "b", "c"]);
            assert_eq!(leaf.active, TabIndex(1));
        }
    }

    #[test]
    fn same_leaf_reorder_changes_tab_order() {
        let mut dock = DockState::new(vec!["a", "b", "c"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        assert!(dock.move_tab(
            TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0)),
            TabDestination::Node(root, TabInsert::Append),
        ));
        assert_eq!(dock.leaf(root).unwrap().tabs, ["b", "c", "a"]);
    }

    #[test]
    fn same_leaf_insert_uses_pre_removal_destination_slots() {
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        for (from, to, expected) in [
            (0, 2, ["b", "a", "c"]),
            (0, 3, ["b", "c", "a"]),
            (2, 0, ["c", "a", "b"]),
            (2, 1, ["a", "c", "b"]),
        ] {
            let mut dock = DockState::new(vec!["a", "b", "c"]);
            assert!(dock.move_tab(
                TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(from)),
                TabDestination::Node(root, TabInsert::Insert(TabIndex(to))),
            ));
            assert_eq!(dock.leaf(root).unwrap().tabs, expected);
        }
    }

    #[test]
    fn rejected_group_move_does_not_consume_source_tabs() {
        let mut dock = DockState::new(vec!["a", "b"]);
        let [left, right] = dock.main_surface_mut().split_right(
            NodeIndex::root(), 0.5, vec!["c"],
        );
        let source = NodePath::new(SurfaceIndex::main(), left);
        let destination = NodePath::new(SurfaceIndex::main(), right);
        assert!(!dock.move_node(
            source,
            TabDestination::Node(destination, TabInsert::Insert(TabIndex(2))),
        ));
        assert_eq!(dock.leaf(source).unwrap().tabs, ["a", "b"]);
        assert_eq!(dock.leaf(destination).unwrap().tabs, ["c"]);
    }

    #[test]
    fn moving_group_between_leaves_preserves_active_member() {
        let mut dock = DockState::new(vec!["a", "b"]);
        let [left, right] = dock.main_surface_mut().split_right(
            NodeIndex::root(), 0.5, vec!["c"],
        );
        let source = NodePath::new(SurfaceIndex::main(), left);
        let destination = NodePath::new(SurfaceIndex::main(), right);
        dock.leaf_mut(source).unwrap().active = TabIndex(1);
        assert!(dock.move_node(source, TabDestination::Node(destination, TabInsert::Append)));
        let leaf = dock.leaf(destination).unwrap();
        assert_eq!(leaf.tabs, ["c", "a", "b"]);
        assert_eq!(leaf.active, TabIndex(2));
    }

    #[test]
    fn removing_an_unfocused_surface_preserves_focus() {
        let mut dock = DockState::new(vec!["main"]);
        let focused = dock.add_docked_surface(Tree::new(vec!["focused"]));
        let removed = dock.add_docked_surface(Tree::new(vec!["removed"]));
        dock.set_focused_node_and_surface(NodePath::new(focused, NodeIndex::root()));
        assert_eq!(dock.focused_leaf().unwrap().surface, focused);
        dock.remove_surface(removed);
        assert_eq!(dock.focused_leaf().unwrap().surface, focused);
    }

    #[test]
    fn removing_middle_surface_does_not_reindex_later_surfaces() {
        let mut dock = DockState::new(vec!["main"]);
        let first = dock.add_docked_surface(Tree::new(vec!["first"]));
        let second = dock.add_docked_surface(Tree::new(vec!["second"]));
        let third = dock.add_docked_surface(Tree::new(vec!["third"]));
        dock.set_focused_node_and_surface(NodePath::new(third, NodeIndex::root()));
        let removed = dock.remove_surface(second).unwrap();
        assert!(removed.node_tree().is_some());
        assert!(dock.get_surface(second).unwrap().is_empty());
        assert_eq!(dock.leaf(NodePath::new(first, NodeIndex::root())).unwrap().tabs, ["first"]);
        assert_eq!(dock.leaf(NodePath::new(third, NodeIndex::root())).unwrap().tabs, ["third"]);
        assert_eq!(dock.focused_leaf().unwrap().surface, third);
        let reused = dock.add_docked_surface(Tree::new(vec!["replacement"]));
        assert_eq!(reused, second);
        assert_eq!(dock.leaf(NodePath::new(third, NodeIndex::root())).unwrap().tabs, ["third"]);
    }

    #[test]
    fn removing_focused_surface_returns_focus_to_main() {
        let mut dock = DockState::new(vec!["main"]);
        let focused = dock.add_docked_surface(Tree::new(vec!["focused"]));
        dock.set_focused_node_and_surface(NodePath::new(focused, NodeIndex::root()));
        dock.remove_surface(focused);
        assert_eq!(dock.focused_leaf().unwrap().surface, SurfaceIndex::main());
    }

    #[test]
    fn retain_none_then_push() {
        let mut t = DockState::new(vec![]);
        t.push_to_focused_leaf(0);
        let i = t.find_tab(&0).unwrap();
        t.remove_tab(i);
        t.retain_tabs(|_| false);
        t.push_to_focused_leaf(0);
    }
}
