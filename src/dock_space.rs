//! Several named docks sharing one drag: see [`DockSpace`].

use egui::{Id, Rect, ViewportId};

use crate::floats::{FloatEvent, Floating, PendingFloat};
#[cfg(test)]
use crate::floats::WindowMove;
use crate::layout::{
    Content, Drops, Fold, Laid, LaidZone, Layout, Restore, Shown, Zone, ZoneKind, ZoneName,
};
use crate::layout::{Float, FloatHost, Place};
use crate::{AllowedSplits, DockState, NodePath, Surface, SurfaceIndex, TabPath, Tree};

/// Several named docks, each with its own tree, and floating windows, sharing one drag: a tab
/// dragged from any dock to any other gets the same drop zones, preview and veto as inside one
/// dock.
///
/// Each dock is drawn by its own [`DockArea::dock`](crate::DockArea::dock) call, inside the
/// [`Ui`](egui::Ui) the application lays out for it (a side panel, the central panel, a flyout),
/// at any point of the frame. The calls share the drag through the space's [`id`](Self::id).
/// One dock, the [windows dock](Self::set_windows_dock), also draws the floating windows.
///
/// The docks are surfaces of one [`DockState`] ([`Surface::Main`]), so a [`TabPath`]
/// or a [`TabDestination`](crate::TabDestination) names a place in any of them, and
/// [`DockState::move_tab`] moves a tab between them; [`dock_surface`](Self::dock_surface) and
/// [`dock_name`](Self::dock_name) go between a dock's name and its surface.
///
/// ```
/// # use egui_stack::{AllowedSplits, DockArea, DockRules, DockSpace, TabViewer};
/// # struct Viewer;
/// # impl TabViewer for Viewer {
/// #     type Tab = String;
/// #     fn id(&mut self, tab: &mut String) -> egui::Id { egui::Id::unique(tab.as_str()) }
/// #     fn title(&mut self, tab: &mut String) -> egui::WidgetText { tab.as_str().into() }
/// #     fn ui(&mut self, ui: &mut egui::Ui, tab: &mut String) { ui.label(tab.as_str()); }
/// # }
/// let mut space = DockSpace::new();
/// space.add_dock(
///     "left",
///     DockRules::default().splits(AllowedSplits::TopBottomOnly),
///     vec!["Files".to_owned()],
/// );
/// space.add_dock("centre", DockRules::default(), vec!["Editor".to_owned()]);
///
/// # egui::__run_test_ui(|ui| {
/// egui::Panel::left("left").show(ui, |ui| {
///     DockArea::dock(&mut space, "left").show_inside(ui, &mut Viewer);
/// });
/// egui::CentralPanel::default().show(ui, |ui| {
///     DockArea::dock(&mut space, "centre").show_inside(ui, &mut Viewer);
/// });
/// # });
/// ```
///
/// # Serialization
///
/// With the `serde` feature, a space is one value: its [`DockState`], its docks with their
/// rules, and its windows dock. Its [`id`](Self::id) is not saved.
#[derive(Clone, Debug)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize, serde::Serialize),
    serde(bound(
        deserialize = "Tab: serde::Deserialize<'de>",
        serialize = "Tab: serde::Serialize"
    ))
)]
pub struct DockSpace<Tab> {
    state: DockState<Tab>,
    docks: Vec<Dock>,
    windows_dock: Option<String>,
    /// Where tabs moved out to a float came from, for them to go back.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Vec::is_empty")
    )]
    origins: Vec<TabOrigin>,
    #[cfg_attr(
        feature = "serde",
        serde(skip, default = "DockSpace::<()>::default_id")
    )]
    id: Id,
    /// The rects no tab is dropped in, this frame.
    #[cfg_attr(feature = "serde", serde(skip))]
    no_drop: NoDrop,
    /// The floats' side of the space: tabs moved out waiting for their float, the events.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) floating: Floating,
    /// The application's bus, given with [`DockSpace::with_bus`].
    #[cfg(feature = "bus")]
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) bus: Option<crate::bus::SpaceBus<Tab>>,
}

/// Where a tab came from before it moved out to a float: its dock, and the tabs next to it in
/// its leaf, so that closing the float can put it back beside them
/// ([`DockSpace::record_origin`]). Tabs are named by their [`TabViewer::id`](crate::TabViewer::id).
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[non_exhaustive]
pub struct TabOrigin {
    /// The tab.
    pub tab: Id,
    /// The dock it was in.
    pub dock: String,
    /// The tab before it in its leaf, if any.
    pub before: Option<Id>,
    /// The tab after it in its leaf, if any.
    pub after: Option<Id>,
}

impl TabOrigin {
    /// The origin of `tab`: the dock `dock`, between `before` and `after`.
    pub fn new(tab: Id, dock: impl Into<String>, before: Option<Id>, after: Option<Id>) -> Self {
        Self {
            tab,
            dock: dock.into(),
            before,
            after,
        }
    }
}

/// Where a tab was in a dock, for it to go back there: its dock, the tabs next to it in its
/// group, and its index in the group. [`DockSpace::take_with_place`] takes a tab out of the
/// space with its place; [`DockSpace::restore_at`] puts it back. With the `serde` feature it
/// saves with the application's state, and stays valid across a layout save and load: it names
/// tabs by their [`TabViewer::id`](crate::TabViewer::id) and the dock by its name, not by
/// indices.
///
/// The space keeps no trace of a taken tab: between the two calls, the tab is the
/// application's, as is the place. See [`DockSpace::restore_at`] for where the tab goes when
/// its group or its dock is gone.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
#[non_exhaustive]
pub struct TabPlace {
    /// The tab, its dock and its neighbours.
    origin: TabOrigin,
    /// Its index in its group.
    index: usize,
    /// For a tab taken from a float: where it came from before it floated.
    #[cfg_attr(
        feature = "serde",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    float_origin: Option<TabOrigin>,
}

impl TabPlace {
    /// The tab, by its [`TabViewer::id`](crate::TabViewer::id).
    pub fn tab(&self) -> Id {
        self.origin.tab
    }

    /// The name of the dock the tab was in.
    pub fn dock(&self) -> &str {
        &self.origin.dock
    }
}

/// What an application declared no tab may be dropped in ([`DockSpace::no_drop_zone`]): rects,
/// and widgets whose rects egui knows.
#[derive(Clone, Debug, Default)]
pub(crate) struct NoDrop {
    pub(crate) rects: Vec<Rect>,
    pub(crate) ids: Vec<Id>,
    /// Folded zones' strips taking drops for a dock ([`DockSpace::bind`]): the strip, and the
    /// dock's name.
    pub(crate) strips: Vec<StripDrop>,
    /// Collapsed zones' restore bands ([`DockSpace::bind`]), in tree order.
    pub(crate) restores: Vec<RestoreDrop>,
    /// Folded zones and collapsed zones a tab was dropped on, to unfold
    /// ([`DockSpace::take_unfolds`]). Kept across frames until taken.
    pub(crate) unfolds: Vec<ZoneName>,
}

/// A collapsed zone ([`Fold::Collapse`]) a dragged tab may bring back ([`DockSpace::bind`]).
#[derive(Clone, Debug)]
pub(crate) struct RestoreDrop {
    /// The zone, to unfold once a tab is dropped on its band.
    pub(crate) zone: ZoneName,
    /// Where it lay and where it would come back.
    pub(crate) restore: Restore,
    /// The dock a tab released on its band joins.
    pub(crate) dock: String,
}

/// A folded zone's strip taking drops for a dock ([`DockSpace::bind`]).
#[derive(Clone, Debug)]
pub(crate) struct StripDrop {
    pub(crate) rect: Rect,
    /// The dock a tab released there joins.
    pub(crate) dock: String,
    /// The folded zone, to unfold once a tab is dropped there.
    pub(crate) unfold: ZoneName,
}

/// Whether `name` is one the space makes itself for a float's dock (`float-N` for tabs moved out
/// to a float, `window-N` for a former egui_dock window), not one the application gave: such a
/// dock is shown by its float's window title, not by its name.
///
/// So these names are reserved: [`DockSpace::add_dock`] documents it for applications.
pub(crate) fn library_named(name: &str) -> bool {
    ["float-", "window-"].iter().any(|prefix| {
        name.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// A dock of a [`DockSpace`]: its name, its surface and its rules.
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub(crate) struct Dock {
    pub(crate) name: String,
    pub(crate) surface: SurfaceIndex,
    pub(crate) rules: DockRules,
    /// Where it was drawn last, for what counts as outside every dock.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) rect: Option<Rect>,
    /// The viewport it was drawn in last: its rect is in that viewport's points.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) viewport: Option<ViewportId>,
    /// Whether a float of the layout shows it, as the floats were last drawn
    /// ([`FloatsArea`](crate::FloatsArea)).
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) floating: bool,
    /// The layout zone holding it ([`Content::Dock`]), as [`DockSpace::bind`] last read it.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub(crate) zone: Option<ZoneName>,
}

/// What a dock of a [`DockSpace`] allows, set when it is added with [`DockSpace::add_dock`] or
/// later with [`DockSpace::set_rules`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(serde::Deserialize, serde::Serialize),
    serde(default)
)]
#[non_exhaustive]
pub struct DockRules {
    /// The splits a dropped tab may make in this dock, on top of
    /// [`DockArea::allowed_splits`](crate::DockArea::allowed_splits). By default
    /// [`AllowedSplits::All`]; with [`AllowedSplits::None`], a tab dropped in the dock joins a
    /// leaf's tabs.
    pub splits: AllowedSplits,
    /// Whether a tab dragged out of this dock may become a floating window. By default `true`.
    pub windows: bool,
    /// Whether the dock stays in the space, as an empty drop target, once its last tab leaves.
    /// By default `true`; when `false`, the dock is removed from the space as soon as it is
    /// empty.
    pub keep_when_empty: bool,
    /// Whether a dragged tab may be dropped in this dock. By default `true`; when `false`, the
    /// dock is never a drop target, its own tabs included: a tab released over it goes back
    /// where it was, as over a [no-drop zone](DockSpace::no_drop_zone). Its tabs can still be
    /// dragged out of it.
    pub accept_drops: bool,
}

impl Default for DockRules {
    fn default() -> Self {
        Self {
            splits: AllowedSplits::All,
            windows: true,
            keep_when_empty: true,
            accept_drops: true,
        }
    }
}

impl DockRules {
    /// Sets [`splits`](Self::splits).
    #[inline]
    pub fn splits(mut self, splits: AllowedSplits) -> Self {
        self.splits = splits;
        self
    }

    /// Sets [`windows`](Self::windows).
    #[inline]
    pub fn windows(mut self, windows: bool) -> Self {
        self.windows = windows;
        self
    }

    /// Sets [`keep_when_empty`](Self::keep_when_empty).
    #[inline]
    pub fn keep_when_empty(mut self, keep_when_empty: bool) -> Self {
        self.keep_when_empty = keep_when_empty;
        self
    }

    /// Sets [`accept_drops`](Self::accept_drops).
    #[inline]
    pub fn accept_drops(mut self, accept_drops: bool) -> Self {
        self.accept_drops = accept_drops;
        self
    }
}

impl DockSpace<()> {
    /// The id of a [`DockSpace`] whose [`id`](DockSpace::id) was not set.
    pub fn default_id() -> Id {
        Id::unique("egui_stack::DockSpace")
    }
}

impl<Tab> Default for DockSpace<Tab> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Tab> DockSpace<Tab> {
    /// A space with no dock yet.
    pub fn new() -> Self {
        Self {
            state: DockState::new(vec![]),
            docks: Vec::new(),
            windows_dock: None,
            origins: Vec::new(),
            id: DockSpace::default_id(),
            no_drop: NoDrop::default(),
            floating: Floating::default(),
            #[cfg(feature = "bus")]
            bus: None,
        }
    }

    /// Takes several [`DockState`]s, one per dock, as an application drawing one
    /// [`DockArea`](crate::DockArea) per area had them: each main surface becomes the dock of
    /// that name, with those rules, and their windows become the space's. The translations are
    /// the first state's.
    ///
    /// # Panics
    ///
    /// Panics if two docks have the same name.
    pub fn from_states(
        states: impl IntoIterator<Item = (String, DockRules, DockState<Tab>)>,
    ) -> Self {
        let mut space = Self::new();
        let mut windows = Vec::new();
        for (index, (name, rules, state)) in states.into_iter().enumerate() {
            if index == 0 {
                space.state.translations = state.translations.clone();
            }
            for surface in state.into_surfaces() {
                match surface {
                    Surface::Main(tree) => {
                        space.add_dock_tree(name.clone(), rules, tree);
                    }
                    Surface::Window(tree, window) => windows.push((tree, window)),
                    Surface::Empty => (),
                }
            }
        }
        for (tree, window) in windows {
            let surface = space.state.add_window(vec![]);
            if let Some(Surface::Window(empty, state)) = space.state.get_surface_mut(surface) {
                *empty = tree;
                *state = window;
            }
        }
        space
    }

    /// Sets the id the space's [`DockArea`](crate::DockArea)s share, which keeps their drag. Two
    /// spaces shown at once need different ids. By default it's [`DockSpace::default_id`].
    #[inline]
    pub fn with_id(mut self, id: Id) -> Self {
        self.id = id;
        self
    }

    /// Sets the id the space's [`DockArea`](crate::DockArea)s share; see [`with_id`](Self::with_id).
    #[inline]
    pub fn set_id(&mut self, id: Id) {
        self.id = id;
    }

    /// The id the space's [`DockArea`](crate::DockArea)s share.
    #[inline]
    pub fn id(&self) -> Id {
        self.id
    }

    /// Adds a dock named `name` holding `tabs` in one leaf (none for an empty dock), and returns
    /// its surface.
    ///
    /// The names `float-<digits>` and `window-<digits>` (`float-3`, `window-1`) are reserved for
    /// the docks the space makes itself for floats ([`float_out`](Self::float_out), a tab dragged
    /// out of every dock, [`windows_to_floats`](Self::windows_to_floats)): such a dock is
    /// recognised by its name and shown by its float's window title where
    /// [`TabViewer::dock_title`](crate::TabViewer::dock_title) gives none. Give a dock another
    /// name to have it shown by that name.
    ///
    /// # Panics
    ///
    /// Panics if the space already has a dock of that name.
    pub fn add_dock(
        &mut self,
        name: impl Into<String>,
        rules: DockRules,
        tabs: Vec<Tab>,
    ) -> SurfaceIndex {
        // An empty dock has no leaf: the whole dock is the drop target.
        let tree = if tabs.is_empty() {
            Tree::default()
        } else {
            Tree::new(tabs)
        };
        self.add_dock_tree(name, rules, tree)
    }

    /// Adds a dock named `name` holding `tree`, and returns its surface.
    ///
    /// Dock names `float-<digits>` and `window-<digits>` are reserved for the space's own float
    /// docks: see [`add_dock`](Self::add_dock).
    ///
    /// # Panics
    ///
    /// Panics if the space already has a dock of that name.
    pub fn add_dock_tree(
        &mut self,
        name: impl Into<String>,
        rules: DockRules,
        tree: Tree<Tab>,
    ) -> SurfaceIndex {
        let name = name.into();
        assert!(
            !self.has_dock(&name),
            "the space already has a dock named {name:?}"
        );
        // The main surface is the first dock's, until that dock is removed.
        let main_free = !self
            .docks
            .iter()
            .any(|dock| dock.surface == SurfaceIndex::main());
        let surface = if main_free {
            *self.state.main_surface_mut() = tree;
            SurfaceIndex::main()
        } else {
            self.state.add_docked_surface(tree)
        };
        self.docks.push(Dock {
            name,
            surface,
            rules,
            rect: None,
            viewport: None,
            floating: false,
            zone: None,
        });
        surface
    }

    /// Removes the dock named `name` from the space, and returns its tree.
    pub fn remove_dock(&mut self, name: &str) -> Option<Tree<Tab>> {
        let index = self.dock_index(name)?;
        let surface = self.docks[index].surface;
        // A missing or non-docked surface is not a removable dock. Do not
        // unregister its name until the corresponding tree is available.
        if !matches!(self.state.get_surface(surface), Some(Surface::Main(_))) {
            return None;
        }
        let Surface::Main(tree) = self.state.take_surface(surface)? else {
            unreachable!("validated dock surface changed without mutation");
        };
        self.docks.remove(index);
        Some(tree)
    }

    /// Whether the space has a dock named `name`.
    #[inline]
    pub fn has_dock(&self, name: &str) -> bool {
        self.dock_index(name).is_some()
    }

    /// The names of the docks, in the order they were added.
    pub fn dock_names(&self) -> impl Iterator<Item = &str> {
        self.docks.iter().map(|dock| dock.name.as_str())
    }

    /// The surface of the dock named `name`.
    pub fn dock_surface(&self, name: &str) -> Option<SurfaceIndex> {
        self.docks
            .iter()
            .find(|dock| dock.name == name)
            .map(|dock| dock.surface)
    }

    /// The name of the dock whose surface is `surface`; `None` for a floating window.
    pub fn dock_name(&self, surface: SurfaceIndex) -> Option<&str> {
        dock_name(&self.docks, surface)
    }

    /// The tree of the dock named `name`.
    pub fn dock(&self, name: &str) -> Option<&Tree<Tab>> {
        let surface = self.dock_surface(name)?;
        self.state.get_surface(surface)?.node_tree()
    }

    /// The tree of the dock named `name`, mutably.
    pub fn dock_mut(&mut self, name: &str) -> Option<&mut Tree<Tab>> {
        let surface = self.dock_surface(name)?;
        self.state.get_surface_mut(surface)?.node_tree_mut()
    }

    /// The rules of the dock named `name`.
    pub fn rules(&self, name: &str) -> Option<DockRules> {
        self.docks
            .iter()
            .find(|dock| dock.name == name)
            .map(|dock| dock.rules)
    }

    /// Sets the rules of the dock named `name`, for example again after loading a saved space.
    /// Returns `false` if there is no such dock.
    pub fn set_rules(&mut self, name: &str, rules: DockRules) -> bool {
        match self.docks.iter_mut().find(|dock| dock.name == name) {
            Some(dock) => {
                dock.rules = rules;
                true
            }
            None => false,
        }
    }

    /// Declares a rect no dragged tab may be dropped in, for this frame: the application's
    /// toolbar, its status bar, any part of the window that is not a dock. A tab released there
    /// goes back where it was: it does not become a floating window, and no drop preview shows
    /// there. A floating window over the zone still takes the tab.
    ///
    /// The zones hold for one frame: declare them every frame, before or after drawing the docks
    /// (declared after, they take effect a frame later, which a drag does not notice). The space
    /// forgets them once its docks have read them.
    ///
    /// ```
    /// # use egui_stack::{DockRules, DockSpace};
    /// # let mut space = DockSpace::<String>::new();
    /// # let toolbar = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 30.0));
    /// space.no_drop_zone(toolbar);
    /// ```
    pub fn no_drop_zone(&mut self, rect: Rect) {
        self.no_drop.rects.push(rect);
    }

    /// Declares several rects no dragged tab may be dropped in, for this frame; see
    /// [`no_drop_zone`](Self::no_drop_zone).
    pub fn no_drop_zones(&mut self, rects: impl IntoIterator<Item = Rect>) {
        self.no_drop.rects.extend(rects);
    }

    /// Declares a widget, by its [`Id`], no dragged tab may be dropped on, for this frame: its
    /// rect is the one egui knows for it ([`egui::Context::read_response`]), so a panel or a bar
    /// that moves needs no rect of the application's. See [`no_drop_zone`](Self::no_drop_zone).
    pub fn no_drop_widget(&mut self, id: Id) {
        self.no_drop.ids.push(id);
    }

    /// Reads where a [`LayoutArea`](crate::layout::LayoutArea) put its zones this frame, and
    /// makes no-drop zones ([`no_drop_zone`](Self::no_drop_zone)) of those no tab may land in:
    /// a zone with [`Drops::Refuse`], a zone holding no dock ([`Content::App`], or a
    /// [`Content::Dock`] this space has not), and a folded zone's strip. A zone with
    /// [`Content::Empty`] that accepts drops stays outside every dock: a tab released there
    /// becomes a floating window.
    ///
    /// A folded zone with [`Constraints::strip_drops`](crate::layout::Constraints::strip_drops)
    /// that accepts drops, holding a dock of this space, takes them on its strip instead: a tab
    /// released there joins that dock (its focused leaf, else its first), the strip highlighted
    /// meanwhile, unless the dock's rules or the viewer's veto refuse it. A folded split zone
    /// with `strip_drops` takes them for one of the docks it holds, the last active one, else the
    /// first accepting drops. Either way the zone is reported to unfold
    /// ([`take_unfolds`](Self::take_unfolds)), and stays folded until the application unfolds it.
    ///
    /// A collapsed zone ([`Fold::Collapse`]) that accepts drops,
    /// holding a dock of this space that accepts them (for a split, one of its docks, chosen as
    /// for a strip), shows a restore band while a tab is dragged: a thin band at its former
    /// boundary ([`Restore::line`]), [`OverlayFeel::restore_band`](crate::OverlayFeel::restore_band)
    /// wide, on the side it would come back on. Bands on the same boundary stack in tree order.
    /// The band wins over whatever lies under it, no-drop zones included, and only inside
    /// itself; hovered, it previews the rect the zone would take back ([`Restore::rect`]). A tab
    /// released there joins the dock, unless the dock's rules or the viewer's veto refuse it,
    /// and the zone is reported to unfold ([`take_unfolds`](Self::take_unfolds)). Released
    /// anywhere else, or cancelled, the layout is left as it was.
    ///
    /// Call it every frame, after the layout is drawn: its zones take effect for the docks drawn
    /// next, a frame later for those already drawn, which a drag does not notice.
    ///
    /// ```
    /// # use egui_stack::{DockArea, DockSpace, DockRules, TabViewer};
    /// # use egui_stack::layout::{Content, Dir, Drops, Layout, LayoutArea, Zone, ZoneName, ZoneViewer};
    /// # struct Tabs;
    /// # impl TabViewer for Tabs {
    /// #     type Tab = String;
    /// #     fn id(&mut self, tab: &mut String) -> egui::Id { egui::Id::unique(tab.as_str()) }
    /// #     fn title(&mut self, tab: &mut String) -> egui::WidgetText { tab.as_str().into() }
    /// #     fn ui(&mut self, ui: &mut egui::Ui, tab: &mut String) { ui.label(tab.as_str()); }
    /// # }
    /// struct Viewer<'a> {
    ///     space: &'a mut DockSpace<String>,
    /// }
    ///
    /// impl ZoneViewer for Viewer<'_> {
    ///     fn ui(&mut self, ui: &mut egui::Ui, zone: &ZoneName) {
    ///         match zone.0.as_str() {
    ///             "toolbar" => { ui.label("Toolbar"); }
    ///             dock => { DockArea::dock(self.space, dock).show_inside(ui, &mut Tabs); }
    ///         }
    ///     }
    /// }
    ///
    /// let mut space = DockSpace::new();
    /// space.add_dock("centre", DockRules::default(), vec!["Editor".to_owned()]);
    /// let mut layout = Layout::new(Zone::split("root", Dir::Vertical, vec![
    ///     Zone::leaf("toolbar", Content::App),
    ///     Zone::leaf("centre", Content::Dock("centre".into())).with(|c| c.grow = true),
    /// ]));
    /// # egui::__run_test_ui(|ui| {
    /// let laid = LayoutArea::new(&mut layout).show_inside(ui, &mut Viewer { space: &mut space });
    /// space.bind(&laid);
    /// # });
    /// ```
    pub fn bind(&mut self, laid: &Laid) {
        #[cfg(feature = "bus")]
        for event in &laid.events {
            self.emit_change(crate::bus::LayoutChange::of(event));
        }
        self.floating.zones = crate::floats::BoundZones::of(laid);
        for zone in &laid.zones {
            if let Some(Content::Dock(name)) = &zone.content
                && let Some(dock) = self.docks.iter_mut().find(|dock| dock.name == *name)
            {
                dock.zone = Some(zone.name.clone());
            }
            if let (Shown::Collapsed, Some(restore)) = (zone.shown, zone.restore) {
                if let Some(dock) = self.restore_dock(zone) {
                    self.no_drop.restores.push(RestoreDrop {
                        zone: zone.name.clone(),
                        restore,
                        dock,
                    });
                }
                continue;
            }
            let Some(content) = &zone.content else {
                // A folded split zone's strip: a target for one of its docks, else a no-drop
                // zone, as a folded leaf's strip is.
                if let Some(strip) = zone.strip {
                    match self.split_strip_dock(zone) {
                        Some(dock) => self.no_drop.strips.push(StripDrop {
                            rect: strip,
                            dock,
                            unfold: zone.name.clone(),
                        }),
                        None => self.no_drop.rects.push(strip),
                    }
                }
                continue;
            };
            if let Some(strip) = zone.strip {
                let takes = match content {
                    Content::Dock(name) => {
                        zone.strip_drops
                            && zone.drops == Drops::Accept
                            && matches!(zone.shown, Shown::Folded | Shown::AutoFolded)
                            && self.rules(name).is_some_and(|rules| rules.accept_drops)
                    }
                    _ => false,
                };
                match content {
                    Content::Dock(name) if takes => {
                        self.no_drop.strips.push(StripDrop {
                            rect: strip,
                            dock: name.clone(),
                            unfold: zone.name.clone(),
                        });
                    }
                    _ => self.no_drop.rects.push(strip),
                }
            }
            if !matches!(zone.shown, Shown::Open | Shown::Over) {
                continue;
            }
            let no_dock = match content {
                Content::Dock(name) => !self.has_dock(name),
                Content::App => true,
                Content::Empty => false,
            };
            if no_dock || zone.drops == Drops::Refuse {
                self.no_drop.rects.push(zone.rect);
            }
        }
    }

    /// The dock a folded split zone's strip takes drops for, with
    /// [`Constraints::strip_drops`](crate::layout::Constraints::strip_drops): of the docks the
    /// split holds that accept drops, the one holding the focused leaf (the last active), else
    /// the first.
    fn split_strip_dock(&self, zone: &LaidZone) -> Option<String> {
        if !(zone.strip_drops
            && zone.drops == Drops::Accept
            && matches!(zone.shown, Shown::Folded | Shown::AutoFolded))
        {
            return None;
        }
        self.split_dock(zone)
    }

    /// The dock a collapsed zone's restore band takes drops for: the dock a leaf holds, or one
    /// of a split's, chosen as for a strip ([`split_dock`](Self::split_dock)); none when the
    /// zone or the dock refuses drops.
    fn restore_dock(&self, zone: &LaidZone) -> Option<String> {
        if zone.drops != Drops::Accept {
            return None;
        }
        match &zone.content {
            Some(Content::Dock(name)) => self
                .rules(name)
                .is_some_and(|rules| rules.accept_drops)
                .then(|| name.clone()),
            Some(_) => None,
            None => self.split_dock(zone),
        }
    }

    /// Of the docks a split zone holds that accept drops, the one holding the focused leaf (the
    /// last active), else the first.
    fn split_dock(&self, zone: &LaidZone) -> Option<String> {
        let candidates: Vec<&String> = zone
            .docks
            .iter()
            .filter(|name| self.rules(name).is_some_and(|rules| rules.accept_drops))
            .collect();
        let focused = self.state.focused_leaf().map(|path| path.surface);
        candidates
            .iter()
            .find(|name| focused.is_some() && self.dock_surface(name) == focused)
            .or(candidates.first())
            .map(|name| (*name).clone())
    }

    /// The zones a tab was dropped on since the last call, for the application to unfold
    /// ([`Layout::set_folded`](crate::layout::Layout::set_folded)): folded zones, whether they
    /// hold one dock or split into several, through their strip, which takes drops with
    /// [`Constraints::strip_drops`](crate::layout::Constraints::strip_drops); and collapsed zones,
    /// through their restore band ([`bind`](Self::bind)). egui_stack unfolds none of them itself:
    /// an application that keeps a zone folded on a drop leaves it out. With the `bus` feature,
    /// the drop's `stack.dock.drop` ask and its `TabMoved` change name the zone too
    /// (`DropAsk::zone`, `LayoutChange::zone`). Call it every frame, after the layout is drawn.
    pub fn take_unfolds(&mut self) -> Vec<ZoneName> {
        std::mem::take(&mut self.no_drop.unfolds)
    }

    /// Collapses the zones of `layout` that collapse ([`Fold::Collapse`]) and whose docks are all
    /// empty: each holds at least one dock ([`Content::Dock`]), and every dock it holds is a dock
    /// of this space with no tab. Zones hidden, already folded, or that grow are left alone, and
    /// so are those under a zone it collapses. Returns the zones it collapsed, parents first.
    ///
    /// Call it every frame, before the layout is drawn: a dock whose last tab leaves gives its
    /// room to its neighbours, and a tab dropped on its restore band brings it back
    /// ([`bind`](Self::bind), [`take_unfolds`](Self::take_unfolds)). A dock that should stay
    /// collapsible needs [`DockRules::keep_when_empty`], the default.
    pub fn collapse_empty(&self, layout: &mut Layout) -> Vec<ZoneName> {
        fn visit<Tab>(space: &DockSpace<Tab>, zone: &mut Zone, out: &mut Vec<ZoneName>) {
            if zone.state.hidden || zone.state.folded {
                return;
            }
            if zone.constraints.fold == Fold::Collapse && !zone.constraints.grow {
                let mut docks = Vec::new();
                zone.visit(&mut |under| {
                    if let ZoneKind::Leaf {
                        content: Content::Dock(name),
                    } = &under.kind
                    {
                        docks.push(name.clone());
                    }
                });
                let empty = !docks.is_empty()
                    && docks
                        .iter()
                        .all(|name| space.dock(name).is_some_and(Tree::is_empty));
                if empty {
                    zone.state.folded = true;
                    zone.state.over = false;
                    out.push(zone.name.clone());
                    return;
                }
            }
            if let ZoneKind::Split { children, .. } = &mut zone.kind {
                for child in children {
                    visit(space, child, out);
                }
            }
        }
        let mut collapsed = Vec::new();
        visit(self, &mut layout.root, &mut collapsed);
        collapsed
    }

    /// The dock whose [`DockArea`](crate::DockArea) also draws the floating windows: the one
    /// set with [`set_windows_dock`](Self::set_windows_dock) if the space has it, otherwise the
    /// first dock.
    #[deprecated(
        since = "0.28.0",
        note = "legacy floating windows: draw the space's floats with `FloatsArea`, which every dock's tabs move out to"
    )]
    pub fn windows_dock(&self) -> Option<&str> {
        windows_dock(&self.docks, self.windows_dock.as_deref())
    }

    /// Sets which dock's [`DockArea`](crate::DockArea) also draws the floating windows.
    #[deprecated(
        since = "0.28.0",
        note = "legacy floating windows: draw the space's floats with `FloatsArea`, which every dock's tabs move out to"
    )]
    pub fn set_windows_dock(&mut self, name: impl Into<String>) {
        self.windows_dock = Some(name.into());
    }

    /// The state holding every dock's tree and the floating windows: to find a tab, read a
    /// window, or anything a [`DockState`] gives.
    #[inline]
    pub fn state(&self) -> &DockState<Tab> {
        &self.state
    }

    /// The state holding every dock's tree and the floating windows, mutably: to move, close or
    /// focus a tab, open a window, or anything a [`DockState`] does. A dock's surface stays a
    /// dock's: use [`remove_dock`](Self::remove_dock) to remove one.
    #[inline]
    pub fn state_mut(&mut self) -> &mut DockState<Tab> {
        &mut self.state
    }

    /// Returns a new [`DockSpace`] while mapping and filtering the tab type; see
    /// [`DockState::filter_map_tabs`].
    pub fn filter_map_tabs<F, NewTab>(&self, function: F) -> DockSpace<NewTab>
    where
        F: FnMut(&Tab) -> Option<NewTab>,
    {
        DockSpace {
            state: self.state.filter_map_tabs(function),
            docks: self.docks.clone(),
            windows_dock: self.windows_dock.clone(),
            origins: self.origins.clone(),
            id: self.id,
            no_drop: self.no_drop.clone(),
            floating: self.floating.clone(),
            // The bus names tabs by a function of the old tab type.
            #[cfg(feature = "bus")]
            bus: None,
        }
    }

    /// Returns a new [`DockSpace`] while mapping the tab type; see [`DockState::map_tabs`].
    pub fn map_tabs<F, NewTab>(&self, mut function: F) -> DockSpace<NewTab>
    where
        F: FnMut(&Tab) -> NewTab,
    {
        self.filter_map_tabs(move |tab| Some(function(tab)))
    }

    /// Records where the tab at `path` is, in a dock, before it moves out to a float: the
    /// dock and its neighbours in the leaf, named by `id_of` (the viewer's
    /// [`TabViewer::id`](crate::TabViewer::id)). It replaces the tab's previous origin. Returns
    /// it, or `None` when `path` is not a tab of a dock (a tab of a floating window keeps the
    /// origin it had).
    pub fn record_origin(
        &mut self,
        path: TabPath,
        mut id_of: impl FnMut(&mut Tab) -> Id,
    ) -> Option<&TabOrigin> {
        record_origin(
            &mut self.state,
            &self.docks,
            &mut self.origins,
            path,
            &mut id_of,
        )?;
        self.origins.last()
    }

    /// Where the tab `tab` came from, if it moved out to a float and its origin was recorded.
    pub fn origin(&self, tab: Id) -> Option<&TabOrigin> {
        self.origins.iter().find(|origin| origin.tab == tab)
    }

    /// Forgets where the tab `tab` came from (it went back, or closed), and returns it.
    pub fn take_origin(&mut self, tab: Id) -> Option<TabOrigin> {
        let index = self.origins.iter().position(|origin| origin.tab == tab)?;
        Some(self.origins.remove(index))
    }

    /// Every origin recorded.
    pub fn origins(&self) -> &[TabOrigin] {
        &self.origins
    }

    /// Takes the tab `tab` (named by `id_of`, the viewer's
    /// [`TabViewer::id`](crate::TabViewer::id)) out of its dock, with its place: the dock, the
    /// tabs next to it and its index in its group. Give both to
    /// [`restore_at`](Self::restore_at) to put it back. Returns `None` when no dock of the space
    /// has the tab.
    ///
    /// Use it to hide a tab and show it again, or to close a tab and reopen it where it was:
    /// the space keeps nothing of the tab, so it draws, counts, saves and focuses as if the tab
    /// had closed. A group emptied goes, as when its last tab closes; a zone emptied stays,
    /// unless its dock says otherwise ([`DockRules::keep_when_empty`],
    /// [`collapse_empty`](Self::collapse_empty)). A tab taken from a float takes the float's
    /// own origin ([`origin`](Self::origin)) with it, and the float closes once empty, as
    /// [`ClosePolicy::ReturnToOrigin`](crate::ClosePolicy::ReturnToOrigin) would leave it.
    ///
    /// ```
    /// # use egui_stack::{DockRules, DockSpace, TabPlace};
    /// # let id_of = |tab: &mut String| egui::Id::unique(tab.as_str());
    /// let mut space = DockSpace::new();
    /// space.add_dock("main", DockRules::default(), vec!["Files".to_owned(), "Log".to_owned()]);
    ///
    /// // Hide "Files": the application keeps the tab and its place.
    /// let (tab, place): (String, TabPlace) =
    ///     space.take_with_place(egui::Id::unique("Files"), id_of).unwrap();
    /// assert_eq!(space.dock("main").unwrap().num_tabs(), 1);
    ///
    /// // Show it again: back before "Log".
    /// assert_eq!(space.restore_at(tab, place, id_of).ok(), Some("main".to_owned()));
    /// ```
    pub fn take_with_place(
        &mut self,
        tab: Id,
        mut id_of: impl FnMut(&mut Tab) -> Id,
    ) -> Option<(Tab, TabPlace)> {
        let surfaces: Vec<SurfaceIndex> = self.docks.iter().map(|dock| dock.surface).collect();
        let path = self.state.iter_all_tabs_mut().find_map(|(path, each)| {
            (surfaces.contains(&path.surface) && id_of(each) == tab).then_some(path)
        })?;
        let mut recorded = Vec::new();
        record_origin(
            &mut self.state,
            &self.docks,
            &mut recorded,
            path,
            &mut id_of,
        )?;
        let origin = recorded.pop()?;
        let floating = self
            .docks
            .iter()
            .any(|dock| dock.surface == path.surface && dock.floating);
        // Do not consume the saved origin until the tab has actually been
        // removed. A failed removal must leave both trees and metadata intact.
        let taken = self.state.remove_tab(path)?;
        let float_origin = if floating {
            self.take_origin(tab)
        } else {
            None
        };
        Some((
            taken,
            TabPlace {
                origin,
                index: path.tab.0,
                float_origin,
            },
        ))
    }

    /// Puts `tab`, taken with [`take_with_place`](Self::take_with_place), back at `place`, and
    /// returns the name of the dock it went to. `id_of` names tabs as for
    /// [`take_with_place`](Self::take_with_place).
    ///
    /// The tab goes where a float's tab goes when the float closes
    /// ([`ClosePolicy::ReturnToOrigin`](crate::ClosePolicy::ReturnToOrigin)):
    /// - in its dock, beside the tab it was next to (after the one before it, else before the
    ///   one after it), in whichever of the dock's groups that tab is now: its group comes back
    ///   through its neighbours;
    /// - when neither neighbour is in the dock any more (the tab was alone in its group, or the
    ///   whole group went), in the dock's focused (or first) group, at its old index (or last);
    ///   in an empty dock, as its only tab;
    /// - when its dock is gone: for a tab taken from a float, where it came from before it
    ///   floated, as above; else in the first dock no float shows, after its focused (or
    ///   first) group's tabs.
    ///
    /// The tab becomes its group's active tab. A tab put back in a float keeps the origin it
    /// had there. Returns the tab when the space has no dock to put it in.
    pub fn restore_at(
        &mut self,
        tab: Tab,
        place: TabPlace,
        mut id_of: impl FnMut(&mut Tab) -> Id,
    ) -> Result<String, Tab> {
        let TabPlace {
            origin,
            index,
            float_origin,
        } = place;
        let near = |origin: &TabOrigin, index: Option<usize>| Near {
            before: origin.before,
            after: origin.after,
            index,
        };
        // A dock name is not a usable restoration destination unless its
        // registered surface still resolves to a dock tree.
        let usable = |name: &str| {
            self.dock_surface(name)
                .and_then(|surface| self.state.get_surface(surface))
                .is_some_and(|surface| matches!(surface, Surface::Main(_)))
        };
        let target = if usable(&origin.dock) {
            Some((origin.dock.clone(), Some(near(&origin, Some(index)))))
        } else if let Some(from) = float_origin
            .as_ref()
            .filter(|from| usable(&from.dock))
        {
            Some((from.dock.clone(), Some(near(from, None))))
        } else {
            self.docks
                .iter()
                .find(|dock| !dock.floating && usable(&dock.name))
                .map(|dock| (dock.name.clone(), None))
        };
        let Some((dock, near)) = target else {
            return Err(tab);
        };
        put_tab(self, &mut id_of, &dock, tab, near)?;
        let floating = self
            .docks
            .iter()
            .any(|each| each.name == dock && each.floating);
        if let Some(from) = float_origin.filter(|_| floating) {
            self.origins.retain(|kept| kept.tab != from.tab);
            self.origins.push(from);
        }
        Ok(dock)
    }

    /// Turns each floating window of the space ([`Surface::Window`], as `egui::Window`s) into a
    /// dock of its own and a [`FloatHost::Native`] float showing it, keyed by the dock's name
    /// (`window-1`, `window-2`… skipping names taken). The float's place is the window's last
    /// rect, in points, on no screen in particular ([`Place::screen`] `None`): a
    /// [`Place::repair`] puts it on the first screen. Its minimized flag is kept.
    ///
    /// [`Docked`](crate::Docked) does this when it loads a layout saved before floats (layout
    /// version 1); call it to move a space kept apart from its layout.
    pub fn windows_to_floats(&mut self) -> Vec<Float> {
        let windows: Vec<SurfaceIndex> = self
            .state
            .iter_surfaces_indexed()
            .filter(|(_, surface)| matches!(surface, Surface::Window(..)))
            .map(|(index, _)| index)
            .collect();
        let mut floats = Vec::new();
        let mut number = 0;
        for surface in windows {
            let Some(Surface::Window(tree, window)) = self.state.take_surface(surface) else {
                continue;
            };
            let name = loop {
                number += 1;
                let name = format!("window-{number}");
                if !self.has_dock(&name) {
                    break name;
                }
            };
            let rect = window.shown_rect().unwrap_or_else(|| {
                Rect::from_min_size(
                    window.position().unwrap_or(Place::DEFAULT_POS),
                    Place::DEFAULT_SIZE,
                )
            });
            if let Some(slot) = self.state.get_surface_mut(surface) {
                *slot = Surface::Main(tree);
            }
            self.docks.push(Dock {
                name: name.clone(),
                surface,
                rules: DockRules::default(),
                rect: None,
                viewport: None,
                floating: true,
                zone: None,
            });
            let mut float = Float::dock(name.clone(), name, FloatHost::Native, Place::new(rect));
            float.minimized = window.is_minimized();
            floats.push(float);
        }
        floats
    }

    fn dock_index(&self, name: &str) -> Option<usize> {
        self.docks.iter().position(|dock| dock.name == name)
    }

    /// Moves the tab at `path` out of its dock into a new float, as dragging it outside every
    /// dock does: a dock of its own (`float-N`, which keys the float too), its origin recorded
    /// ([`record_origin`](Self::record_origin), with `id_of` the viewer's
    /// [`TabViewer::id`](crate::TabViewer::id)). The float opens at `rect` (points: in the main
    /// window for [`FloatHost::InWindow`], on the first screen for [`FloatHost::Native`]), hosted
    /// as [`float_host`](Self::float_host) says, at the next
    /// [`FloatsArea::show`](crate::FloatsArea::show). Returns its key, or `None` when there is
    /// no tab at `path`.
    pub fn float_out(
        &mut self,
        path: TabPath,
        rect: Rect,
        mut id_of: impl FnMut(&mut Tab) -> Id,
    ) -> Option<String> {
        self.detach(Detach::Tab(path), rect, &mut id_of)
    }

    /// Moves the whole group (leaf) at `path` out of its dock into a new float, its tabs kept in
    /// order; see [`float_out`](Self::float_out).
    pub fn float_out_group(
        &mut self,
        path: NodePath,
        rect: Rect,
        mut id_of: impl FnMut(&mut Tab) -> Id,
    ) -> Option<String> {
        self.detach(Detach::Group(path), rect, &mut id_of)
    }

    fn detach(
        &mut self,
        source: Detach,
        rect: Rect,
        id_of: &mut dyn FnMut(&mut Tab) -> Id,
    ) -> Option<String> {
        let host = self.floating.host;
        detach(
            &mut self.state,
            &mut self.docks,
            &mut self.origins,
            &mut self.floating,
            source,
            rect,
            None,
            host,
            id_of,
        )
    }

    /// Takes over the floats' state of `old`, a space this one replaces: an application that
    /// rebuilds its space from a layout of its own (rather than changing the docks in place
    /// with [`dock_mut`](Self::dock_mut), [`add_dock`](Self::add_dock) and
    /// [`remove_dock`](Self::remove_dock)) calls it on the new space before drawing it, so that
    /// its open native windows, the drag in progress and the events not taken yet go on.
    ///
    /// Transfers float configuration and unconsumed events. Pending opens and window metadata
    /// are retained only for floating docks that still exist in the replacement space.
    /// A drag session is retained only when its source still matches the replacement state;
    /// old target reports and deferred releases are discarded until the new layout is drawn.
    /// Existing origin records in the replacement take precedence over transferred records.
    ///
    /// The replacement layout is authoritative: this method does not recreate removed docks
    /// or restore their tabs. It replaces the destination's floating runtime state rather
    /// than merging two simultaneously active float managers. Call it while rebuilding the
    /// space, before drawing the new layout, not to combine independent spaces.
    ///
    /// The space ID is not transferred. Preserve it with [`with_id`](Self::with_id) so
    /// viewport and drag handles retain their identity. The old space keeps its docks and
    /// tabs but loses its floating runtime state and saved origins.
    pub fn carry_floats_from(&mut self, old: &mut DockSpace<Tab>) {
        let mut floating = std::mem::take(&mut old.floating);
        if floating
            .session
            .as_ref()
            .is_some_and(|session| !session.source_in(&self.state))
        {
            floating.session = None;
        }
        if let Some(session) = floating.session.as_mut() {
            // Reports describe drop targets in the previous layout. Even when
            // the dragged source survives, their node paths may now identify
            // different targets. Wait for fresh viewport reports before release.
            session.reports.clear();
            session.released = None;
        }
        // Floating metadata keyed by a dock name is only meaningful when the
        // replacement space still owns that floating dock.
        let live_float = |name: &str| {
            self.docks.iter().any(|dock| {
                dock.floating
                    && dock.name == name
                    && self.dock(&dock.name).is_some()
            })
        };
        // Pending opens must not resurrect floats removed by the new layout.
        floating.pending.retain(|pending| live_float(&pending.dock));
        floating.builders.retain(|name, _| live_float(name));
        floating.viewports.retain(|name, _| live_float(name));
        floating.placed.retain(|name, _| live_float(name));
        floating.moving.retain(|name| live_float(name));
        floating.chrome.retain(|name, _| live_float(name));
        floating.collapsed.retain(|name| live_float(name));
        floating.close_asked.retain(|name| live_float(name));
        floating.released_at.retain(|name, _| live_float(name));
        floating.grabs.retain(|name, _| live_float(name));
        if floating
            .window_move
            .as_ref()
            .is_some_and(|moving| !live_float(&moving.key))
        {
            floating.window_move = None;
        }
        // A window's previous hover target and the rectangles/layers of the old
        // layout are geometry from a different frame tree. Keep the gesture's
        // pointer anchor for a surviving float, but require fresh hit testing.
        if let Some(moving) = floating.window_move.as_mut() {
            moving.target = None;
        }
        floating.rects.clear();
        floating.layers.clear();
        floating.zones = crate::floats::BoundZones::default();
        // The new layout may already contain floats with higher identifiers
        // than the previous runtime counter. Never allocate a previously used
        // float name after rebuilding the space.
        for dock in &self.docks {
            if dock.floating {
                if let Some(number) = dock
                    .name
                    .strip_prefix("float-")
                    .and_then(|suffix| suffix.parse::<u32>().ok())
                {
                    floating.next = floating.next.max(number);
                }
            }
        }
        self.floating = floating;
        // The replacement layout's origins take precedence, including when
        // the previous space has multiple stale entries for the same tab.
        for origin in std::mem::take(&mut old.origins) {
            if !self.origins.iter().any(|kept| kept.tab == origin.tab) {
                self.origins.push(origin);
            }
        }
    }

    /// The window events since the last call: floats opened from a tab or a group moved out
    /// of a dock, closed (and where their tabs went), moved by the user, or brought back onto
    /// a screen. They come from [`FloatsArea::show`](crate::FloatsArea::show) and from the
    /// docks' drags; call it once a frame, after drawing.
    pub fn take_float_events(&mut self) -> Vec<FloatEvent> {
        std::mem::take(&mut self.floating.events)
    }

    /// Sets what hosts the floats made from a tab or a group moved out of a dock (released
    /// outside every dock, ejected, or moved to a new window from its menu), once the space's
    /// floats are drawn ([`FloatsArea`](crate::FloatsArea)): [`FloatHost::InWindow`] by
    /// default, [`FloatHost::Native`] for a native window of their own.
    pub fn set_float_host(&mut self, host: FloatHost) {
        self.floating.host = host;
    }

    /// Sets what hosts the floats made from a tab moved out; see
    /// [`set_float_host`](Self::set_float_host).
    #[inline]
    pub fn with_float_host(mut self, host: FloatHost) -> Self {
        self.floating.host = host;
        self
    }

    /// What hosts the floats made from a tab moved out
    /// ([`set_float_host`](Self::set_float_host)).
    pub fn float_host(&self) -> FloatHost {
        self.floating.host
    }

    /// The drag in progress across the space's viewports, if any: what it carries (a tab or a
    /// whole group), the viewport it began in, and the other viewport under the pointer.
    ///
    /// A space has one drag, whichever viewport its docks are drawn in: the main window's, or a
    /// native float's ([`FloatsArea`](crate::FloatsArea)). The viewport it began in keeps the
    /// pointer, even past its edges, and sees the release; every other viewport converts the
    /// pointer to its own points (from the desktop's physical pixels, where the system tells
    /// where windows are), shows where the tab would land, and the move is applied once, by the
    /// viewport the drag began in. Escape cancels it, and so does closing the window under the
    /// pointer. Where window positions are hidden (Wayland), each other window outlines its
    /// docks ([`DockPainter::window_outline`](crate::DockPainter::window_outline)), and the first
    /// that sees the pointer over one of its docks shortly after the release takes the drop.
    pub fn dragging(&self) -> Option<crate::SpaceDrag> {
        self.floating.session.as_ref()?.dragged()
    }

    /// The parts a [`DockArea`](crate::DockArea) showing the dock named `name` borrows.
    pub(crate) fn split_for_area(&mut self, name: &str) -> SpaceParts<'_, Tab> {
        let index = self.dock_index(name);
        let shows_windows = index.is_some()
            && windows_dock(&self.docks, self.windows_dock.as_deref()) == Some(name);
        SpaceParts {
            state: &mut self.state,
            docks: &mut self.docks,
            no_drop: &mut self.no_drop,
            floating: &mut self.floating,
            origins: &mut self.origins,
            #[cfg(feature = "bus")]
            bus: self.bus.as_ref(),
            dock: index,
            shows_windows,
            id: self.id,
        }
    }

    /// The docks, mutably, for the floats' drawing.
    pub(crate) fn docks_mut(&mut self) -> &mut Vec<Dock> {
        &mut self.docks
    }

    /// The docks.
    pub(crate) fn docks(&self) -> &[Dock] {
        &self.docks
    }
}

/// What a [`DockArea`](crate::DockArea) showing a dock of a space borrows of it.
pub(crate) struct SpaceParts<'a, Tab> {
    pub(crate) state: &'a mut DockState<Tab>,
    pub(crate) docks: &'a mut Vec<Dock>,
    pub(crate) no_drop: &'a mut NoDrop,
    pub(crate) floating: &'a mut Floating,
    pub(crate) origins: &'a mut Vec<TabOrigin>,
    #[cfg(feature = "bus")]
    pub(crate) bus: Option<&'a crate::bus::SpaceBus<Tab>>,
    pub(crate) dock: Option<usize>,
    pub(crate) shows_windows: bool,
    pub(crate) id: Id,
}

/// What moves out of a dock to a new float.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Detach {
    /// A tab.
    Tab(TabPath),
    /// A whole group: a leaf and all its tabs.
    Group(NodePath),
}

/// Moves `source` into a new dock (`float-N`), records where its tabs came from (unless they
/// leave another float: they keep the origin they had), and asks for a float showing it at
/// `rect`, `physical` on the desktop when known, hosted by `host`. Returns the new dock's name,
/// which keys the float.
#[allow(clippy::too_many_arguments)]
pub(crate) fn detach<Tab>(
    state: &mut DockState<Tab>,
    docks: &mut Vec<Dock>,
    origins: &mut Vec<TabOrigin>,
    floating: &mut Floating,
    source: Detach,
    rect: Rect,
    physical: Option<Rect>,
    host: crate::layout::FloatHost,
    id_of: &mut dyn FnMut(&mut Tab) -> Id,
) -> Option<String> {
    let (node, tabs) = match source {
        Detach::Tab(path) => {
            let end = path.tab.0.checked_add(1)?;
            (path.node_path(), path.tab.0..end)
        },
        Detach::Group(path) => (path, 0..state.leaf(path).ok()?.tabs.len()),
    };
    let from_float = state.is_window(node.surface)
        || docks
            .iter()
            .any(|dock| dock.surface == node.surface && dock.floating);
    let leaf = state.leaf_mut(node).ok()?;
    if tabs.end > leaf.tabs.len() || tabs.is_empty() {
        return None;
    }
    let ids: Vec<Id> = tabs.clone().map(|i| id_of(&mut leaf.tabs[i])).collect();
    // Stage restoration metadata: a rejected move must leave the existing
    // origin records and dock registry unchanged.
    let mut pending_origins = origins.clone();
    if !from_float {
        for i in tabs {
            let path = TabPath::new(node.surface, node.node, crate::TabIndex(i));
            record_origin(state, docks, &mut pending_origins, path, id_of)?;
        }
    }
    // Do not consume a float identifier until the move has committed.
    let mut next = floating.next;
    let name = loop {
        next = next.checked_add(1)?;
        let name = format!("float-{next}");
        if !docks.iter().any(|dock| dock.name == name) {
            break name;
        }
    };
    let surface = state.add_docked_surface(Tree::default());
    docks.push(Dock {
        name: name.clone(),
        surface,
        // Emptied, it goes, and its float closes with it.
        rules: DockRules::default().keep_when_empty(false),
        rect: None,
        viewport: None,
        floating: true,
        zone: None,
    });
    let moved = match source {
        Detach::Tab(path) => state.move_tab(path, crate::TabDestination::EmptySurface(surface)),
        Detach::Group(path) => state.move_node(path, crate::TabDestination::EmptySurface(surface)),
    };
    if !moved {
        docks.pop();
        // The new dock occupies a previously vacant surface. Leave that slot
        // vacant again without resetting focus on the source dock:
        // remove_surface unconditionally focuses the main surface.
        state.take_surface(surface);
        return None;
    }
    *origins = pending_origins;
    floating.next = next;
    floating.pending.push(PendingFloat {
        dock: name.clone(),
        rect,
        physical,
        tabs: ids,
        host,
    });
    Some(name)
}

/// Records where the tab at `path` is, in a dock of `docks`, in `origins`: see
/// [`DockSpace::record_origin`]. Returns `None` when `path` is not a tab of a dock.
pub(crate) fn record_origin<Tab>(
    state: &mut DockState<Tab>,
    docks: &[Dock],
    origins: &mut Vec<TabOrigin>,
    path: TabPath,
    id_of: &mut dyn FnMut(&mut Tab) -> Id,
) -> Option<()> {
    let dock = dock_name(docks, path.surface)?.to_owned();
    let leaf = state.leaf_mut(path.node_path()).ok()?;
    let at = path.tab.0;
    let tabs = leaf.tabs_mut();
    if at >= tabs.len() {
        return None;
    }
    let tab = id_of(&mut tabs[at]);
    let before = at.checked_sub(1).map(|i| id_of(&mut tabs[i]));
    let after = tabs.get_mut(at + 1).map(&mut *id_of);
    origins.retain(|origin| origin.tab != tab);
    origins.push(TabOrigin::new(tab, dock, before, after));
    Some(())
}

/// Where to put a tab back in a dock ([`put_tab`]): beside its neighbours, else at `index` in
/// the dock's focused (or first) group.
#[derive(Clone, Copy)]
pub(crate) struct Near {
    pub(crate) before: Option<Id>,
    pub(crate) after: Option<Id>,
    pub(crate) index: Option<usize>,
}

/// Puts `tab` in the dock `dock`: after the tab it was next to (`near`), else in the dock's
/// focused (or first) leaf (at `near`'s index, else last), else as the empty dock's only tab.
/// Gives the tab back when the space has no such dock.
pub(crate) fn put_tab<Tab>(
    space: &mut DockSpace<Tab>,
    id_of: &mut dyn FnMut(&mut Tab) -> Id,
    dock: &str,
    tab: Tab,
    near: Option<Near>,
) -> Result<(), Tab> {
    let Some(tree) = space.dock_mut(dock) else {
        return Err(tab);
    };
    if tree.is_empty() {
        *tree = Tree::new(vec![tab]);
        return Ok(());
    }
    if let Some(Near { before, after, .. }) = near {
        let nodes: Vec<_> = tree.breadth_first_index_iter().collect();
        // After the tab before it, wherever it is; else before the tab after it.
        let neighbours = [(before, 1), (after, 0)];
        for (neighbour, offset) in neighbours {
            let Some(neighbour) = neighbour else {
                continue;
            };
            for &node in &nodes {
                let Ok(leaf) = tree.leaf_mut(node) else {
                    continue;
                };
                if let Some(at) = leaf
                    .tabs
                    .iter_mut()
                    .position(|each| id_of(each) == neighbour)
                {
                    leaf.insert_tab(crate::TabIndex(at + offset), tab);
                    return Ok(());
                }
            }
        }
    }
    let leaf = tree
        .focused_leaf()
        .filter(|&node| tree.leaf(node).is_ok())
        .or_else(|| {
            tree.breadth_first_index_iter()
                .find(|&node| tree.leaf(node).is_ok())
        });
    match leaf.and_then(|node| tree.leaf_mut(node).ok()) {
        Some(leaf) => {
            let len = leaf.tabs.len();
            match near.and_then(|near| near.index) {
                Some(index) => leaf.insert_tab(crate::TabIndex(index.min(len)), tab),
                None => leaf.append_tab(tab),
            }
        }
        None => *tree = Tree::new(vec![tab]),
    }
    Ok(())
}

/// The name of the dock of `docks` whose surface is `surface`.
pub(crate) fn dock_name(docks: &[Dock], surface: SurfaceIndex) -> Option<&str> {
    docks
        .iter()
        .find(|dock| dock.surface == surface)
        .map(|dock| dock.name.as_str())
}

/// The windows dock of `docks`: `set`, if it is one of them, otherwise the first.
fn windows_dock<'a>(docks: &'a [Dock], set: Option<&str>) -> Option<&'a str> {
    set.and_then(|set| docks.iter().find(|dock| dock.name == set))
        .or_else(|| docks.first())
        .map(|dock| dock.name.as_str())
}

/// Removes the docks of `docks` that are empty and not kept, emptying their surfaces in `state`.
pub(crate) fn remove_empty_docks<Tab>(state: &mut DockState<Tab>, docks: &mut Vec<Dock>) {
    docks.retain(|dock| {
        let empty = state
            .get_surface(dock.surface)
            .and_then(Surface::node_tree)
            .is_none_or(Tree::is_empty);
        let removed = empty && !dock.rules.keep_when_empty;
        if removed {
            state.take_surface(dock.surface);
        }
        !removed
    });
}

#[cfg(test)]
mod detach_tests {
    use super::*;
    use crate::{NodeIndex, TabIndex};

    fn float_rect() -> Rect {
        Rect::from_min_size(egui::pos2(20.0, 30.0), egui::vec2(400.0, 300.0))
    }

    fn id(tab: &mut &'static str) -> Id {
        Id::new(*tab)
    }

    #[test]
    fn carried_float_counter_never_regresses_below_rebuilt_docks() {
        let mut old = DockSpace::<&'static str>::new();
        old.floating.next = 2;
        let mut replacement = DockSpace::<&'static str>::new();
        replacement.add_dock("editor", DockRules::default(), vec!["one"]);
        replacement.add_dock("float-27", DockRules::default(), vec!["floating"]);
        let index = replacement.dock_index("float-27").unwrap();
        replacement.docks[index].floating = true;
        replacement.carry_floats_from(&mut old);
        assert_eq!(replacement.floating.next, 27);
    }

    #[test]
    fn carrying_origins_prefers_replacement_and_deduplicates_transferred_records() {
        let mut old = DockSpace::<&'static str>::new();
        old.origins.push(TabOrigin::new(Id::new("shared"), "old", None, None));
        old.origins.push(TabOrigin::new(Id::new("new"), "first", None, None));
        old.origins.push(TabOrigin::new(Id::new("new"), "duplicate", None, None));
        let mut replacement = DockSpace::<&'static str>::new();
        replacement.origins.push(TabOrigin::new(Id::new("shared"), "replacement", None, None));

        replacement.carry_floats_from(&mut old);

        assert!(old.origins.is_empty());
        assert_eq!(replacement.origins.len(), 2);
        assert_eq!(replacement.origins[0].dock, "replacement");
        assert_eq!(replacement.origins[1].dock, "first");
    }

    #[test]
    fn carried_pending_float_for_surviving_float_dock_is_preserved() {
        let mut old = DockSpace::<&'static str>::new();
        let mut replacement = DockSpace::<&'static str>::new();
        replacement.add_dock("editor", DockRules::default(), vec!["one"]);
        // A float dock is a normal dock surface with floating ownership.
        replacement.add_dock("float-1", DockRules::default(), vec!["floating"]);
        let index = replacement.dock_index("float-1").unwrap();
        replacement.docks[index].floating = true;
        old.floating.pending.push(PendingFloat {
            dock: "float-1".to_owned(),
            rect: float_rect(),
            physical: None,
            tabs: vec![Id::new("floating")],
            host: FloatHost::InWindow,
        });
        replacement.carry_floats_from(&mut old);
        assert_eq!(replacement.floating.pending.len(), 1);
        assert_eq!(replacement.floating.pending[0].dock, "float-1");
    }

    #[test]
    fn carried_pending_float_without_a_live_dock_is_discarded() {
        let mut old = DockSpace::<&'static str>::new();
        old.add_dock("editor", DockRules::default(), vec!["one"]);
        old.floating.pending.push(crate::floats::PendingFloat {
            dock: "float-1".to_owned(),
            rect: float_rect(),
            physical: None,
            tabs: vec![Id::new("one")],
            host: FloatHost::InWindow,
        });
        let mut replacement = DockSpace::<&'static str>::new();
        replacement.add_dock("editor", DockRules::default(), vec!["one"]);
        replacement.carry_floats_from(&mut old);
        assert!(replacement.floating.pending.is_empty());
        assert!(old.floating.pending.is_empty());
    }

    #[test]
    fn carrying_float_runtime_state_prunes_orphaned_gestures() {
        let mut old = DockSpace::<&'static str>::new();
        old.add_dock("float-1", DockRules::default(), vec!["survives"]);
        old.add_dock("float-2", DockRules::default(), vec!["removed"]);
        for name in ["float-1", "float-2"] {
            let index = old.dock_index(name).unwrap();
            old.docks[index].floating = true;
        }
        old.floating.window_move = Some(WindowMove::new("float-2"));
        old.floating.grabs.insert("float-1".to_owned(), egui::Vec2::splat(4.0));
        old.floating.grabs.insert("float-2".to_owned(), egui::Vec2::splat(8.0));

        let mut replacement = DockSpace::<&'static str>::new();
        replacement.add_dock("float-1", DockRules::default(), vec!["survives"]);
        let index = replacement.dock_index("float-1").unwrap();
        replacement.docks[index].floating = true;

        replacement.carry_floats_from(&mut old);

        assert!(replacement.floating.window_move.is_none());
        assert_eq!(replacement.floating.grabs.len(), 1);
        assert_eq!(
            replacement.floating.grabs.get("float-1"),
            Some(&egui::Vec2::splat(4.0)),
        );
        assert!(!replacement.floating.grabs.contains_key("float-2"));
    }

    #[test]
    fn carried_float_metadata_is_pruned_when_its_dock_disappears() {
        let mut old = DockSpace::<&'static str>::new();
        old.add_dock("editor", DockRules::default(), vec!["one"]);
        old.floating.placed.insert("float-1".to_owned(), egui::pos2(10.0, 20.0));
        old.floating.chrome.insert("float-1".to_owned(), 30.0);
        old.floating.moving.insert("float-1".to_owned());
        old.floating.close_asked.insert("float-1".to_owned());

        let mut replacement = DockSpace::<&'static str>::new();
        replacement.add_dock("editor", DockRules::default(), vec!["one"]);
        replacement.carry_floats_from(&mut old);

        assert!(replacement.floating.placed.is_empty());
        assert!(replacement.floating.chrome.is_empty());
        assert!(replacement.floating.moving.is_empty());
        assert!(replacement.floating.close_asked.is_empty());
        assert!(old.floating.placed.is_empty());
    }

    #[test]
    fn removing_dock_with_missing_surface_does_not_lose_registration() {
        let mut space = DockSpace::new();
        let surface = space.add_dock("editor", DockRules::default(), vec!["one"]);
        space.state.take_surface(surface).unwrap();
        assert!(space.remove_dock("editor").is_none());
        assert_eq!(space.dock_surface("editor"), Some(surface));
        assert_eq!(space.docks.len(), 1);
    }

    #[test]
    fn restore_with_missing_origin_dock_uses_remaining_dock() {
        let mut space = DockSpace::new();
        space.add_dock("original", DockRules::default(), vec!["first", "second"]);
        space.add_dock("fallback", DockRules::default(), vec!["existing"]);
        let (tab, place) = space.take_with_place(Id::new("first"), id).unwrap();
        assert_eq!(tab, "first");
        space.remove_dock("original").unwrap();
        assert_eq!(space.restore_at(tab, place, id).unwrap(), "fallback");
        assert!(space.dock("fallback").unwrap().find_tab(&"first").is_some());
    }

    #[test]
    fn restore_skips_origin_with_stale_surface_registration() {
        let mut space = DockSpace::new();
        let original = space.add_dock("original", DockRules::default(), vec!["first"]);
        space.add_dock("fallback", DockRules::default(), vec!["existing"]);
        let (tab, place) = space.take_with_place(Id::new("first"), id).unwrap();
        space.state.take_surface(original).unwrap();
        assert_eq!(space.dock_surface("original"), Some(original));
        assert_eq!(space.restore_at(tab, place, id).unwrap(), "fallback");
        assert!(space.dock("fallback").unwrap().find_tab(&"first").is_some());
    }

    #[test]
    fn restore_without_usable_dock_returns_original_tab() {
        let mut space = DockSpace::new();
        let original = space.add_dock("original", DockRules::default(), vec!["first"]);
        let (tab, place) = space.take_with_place(Id::new("first"), id).unwrap();
        space.state.take_surface(original).unwrap();
        assert_eq!(space.restore_at(tab, place, id).unwrap_err(), "first");
        assert_eq!(space.dock_surface("original"), Some(original));
    }

    #[test]
    fn restore_uses_surviving_neighbour_before_stale_index() {
        let mut space = DockSpace::new();
        space.add_dock("editor", DockRules::default(), vec!["first", "middle", "last"]);
        let (tab, place) = space.take_with_place(Id::new("middle"), id).unwrap();
        assert_eq!(tab, "middle");

        // The saved index is no longer a reliable position after the first
        // neighbour disappears. The surviving neighbour remains authoritative.
        let first = space.dock("editor").unwrap().find_tab(&"first").unwrap();
        let surface = space.dock_surface("editor").unwrap();
        space.state.remove_tab(TabPath::new(surface, first.0, first.1)).unwrap();
        assert_eq!(space.restore_at(tab, place, id).unwrap(), "editor");
        let tree = space.dock("editor").unwrap();
        let root = tree.leaf(NodeIndex::root()).unwrap();
        assert_eq!(root.tabs, ["middle", "last"]);
    }

    #[test]
    fn restore_skips_registered_docks_with_missing_surfaces() {
        let mut space = DockSpace::new();
        let original = space.add_dock("original", DockRules::default(), vec!["first"]);
        space.add_dock("fallback", DockRules::default(), vec!["existing"]);
        let (tab, place) = space.take_with_place(Id::new("first"), id).unwrap();

        // A registered dock can become unusable when its surface was taken
        // independently. Restoration must select a real destination tree.
        space.state.take_surface(original).unwrap();
        assert!(space.has_dock("original"));
        assert!(space.dock("original").is_none());
        assert_eq!(space.restore_at(tab, place, id).unwrap(), "fallback");
        assert!(space.dock("fallback").unwrap().find_tab(&"first").is_some());
    }

    #[test]
    fn restore_without_any_dock_returns_tab_to_caller() {
        let mut space = DockSpace::new();
        space.add_dock("original", DockRules::default(), vec!["first"]);
        let (tab, place) = space.take_with_place(Id::new("first"), id).unwrap();
        space.remove_dock("original").unwrap();
        assert_eq!(space.restore_at(tab, place, id), Err("first"));
    }

    #[test]
    fn invalid_detach_does_not_register_float_or_change_origins() {
        let mut space = DockSpace::new();
        let surface = space.add_dock("editor", DockRules::default(), vec!["one", "two"]);
        let original_next = space.floating.next;
        let original_docks = space.docks.len();
        let bad = TabPath::new(surface, NodeIndex::root(), TabIndex(99));
        assert!(detach(
            &mut space.state, &mut space.docks, &mut space.origins,
            &mut space.floating, Detach::Tab(bad), float_rect(), None,
            FloatHost::InWindow, &mut id,
        ).is_none());
        assert_eq!(space.docks.len(), original_docks);
        assert_eq!(space.floating.next, original_next);
        assert!(space.floating.pending.is_empty());
        assert!(space.origins.is_empty());
        assert_eq!(space.state[surface][NodeIndex::root()].tabs_count(), 2);
    }

    #[test]
    fn invalid_maximum_tab_index_is_rejected_without_overflow() {
        let mut space = DockSpace::new();
        let surface = space.add_dock("editor", DockRules::default(), vec!["one"]);
        let bad = TabPath::new(surface, NodeIndex::root(), TabIndex(usize::MAX));
        assert!(detach(
            &mut space.state, &mut space.docks, &mut space.origins,
            &mut space.floating, Detach::Tab(bad), float_rect(), None,
            FloatHost::Native, &mut id,
        ).is_none());
        assert_eq!(space.docks.len(), 1);
        assert_eq!(space.floating.next, 0);
        assert!(space.floating.pending.is_empty());
        assert_eq!(space.state[surface][NodeIndex::root()].tabs_count(), 1);
    }

    #[test]
    fn exhausted_float_identifiers_leave_state_unchanged() {
        let mut space = DockSpace::new();
        let surface = space.add_dock("editor", DockRules::default(), vec!["one"]);
        space.floating.next = u32::MAX;
        let path = TabPath::new(surface, NodeIndex::root(), TabIndex(0));
        assert!(detach(
            &mut space.state, &mut space.docks, &mut space.origins,
            &mut space.floating, Detach::Tab(path), float_rect(), None,
            FloatHost::Native, &mut id,
        ).is_none());
        assert_eq!(space.floating.next, u32::MAX);
        assert_eq!(space.docks.len(), 1);
        assert!(space.origins.is_empty());
        assert!(space.floating.pending.is_empty());
        assert_eq!(space.state[surface][NodeIndex::root()].tabs_count(), 1);
    }

    #[test]
    fn detach_tab_commits_origin_and_float_together() {
        let mut space = DockSpace::new();
        let surface = space.add_dock("editor", DockRules::default(), vec!["one", "two"]);
        let path = TabPath::new(surface, NodeIndex::root(), TabIndex(0));
        let name = detach(
            &mut space.state, &mut space.docks, &mut space.origins,
            &mut space.floating, Detach::Tab(path), float_rect(), None,
            FloatHost::InWindow, &mut id,
        ).expect("valid tab should detach");
        let target = space.dock_surface(&name).expect("float dock registered");
        assert_eq!(space.state[target][NodeIndex::root()].tabs_count(), 1);
        assert_eq!(space.state[surface][NodeIndex::root()].tabs_count(), 1);
        assert_eq!(space.origins.len(), 1);
        assert_eq!(space.origins[0].tab, Id::new("one"));
        assert_eq!(space.origins[0].dock, "editor");
        assert_eq!(space.floating.pending.len(), 1);
    }

    #[test]
    fn native_float_detachment_commits_pending_window_and_origin() {
        let mut space = DockSpace::new();
        let surface = space.add_dock("editor", DockRules::default(), vec!["one", "two"]);
        let path = TabPath::new(surface, NodeIndex::root(), TabIndex(0));
        let name = detach(
            &mut space.state, &mut space.docks, &mut space.origins,
            &mut space.floating, Detach::Tab(path), float_rect(), None,
            FloatHost::Native, &mut id,
        ).expect("native float should be queued");
        assert_eq!(space.floating.next, 1);
        assert_eq!(space.floating.pending.len(), 1);
        assert_eq!(space.floating.pending[0].host, FloatHost::Native);
        assert_eq!(space.floating.pending[0].dock, name);
        assert_eq!(space.origins.len(), 1);
        assert_eq!(space.origins[0].dock, "editor");
    }

    #[test]
    fn moving_from_one_float_to_another_retains_original_origin() {
        let mut space = DockSpace::new();
        let surface = space.add_dock("editor", DockRules::default(), vec!["one", "two"]);
        let source = TabPath::new(surface, NodeIndex::root(), TabIndex(0));
        let first = detach(
            &mut space.state, &mut space.docks, &mut space.origins,
            &mut space.floating, Detach::Tab(source), float_rect(), None,
            FloatHost::InWindow, &mut id,
        ).unwrap();
        let first_surface = space.dock_surface(&first).unwrap();
        let second = detach(
            &mut space.state, &mut space.docks, &mut space.origins,
            &mut space.floating,
            Detach::Tab(TabPath::new(first_surface, NodeIndex::root(), TabIndex(0))),
            float_rect(), None, FloatHost::Native, &mut id,
        ).unwrap();
        assert_ne!(first, second);
        assert_eq!(space.floating.pending.len(), 2);
        assert_eq!(space.origins.len(), 1);
        assert_eq!(space.origins[0].dock, "editor");
        assert_eq!(space.origins[0].tab, Id::new("one"));
        let target = space.dock_surface(&second).unwrap();
        assert_eq!(space.state[target][NodeIndex::root()].tabs_count(), 1);
    }

    #[test]
    fn detach_group_preserves_each_tabs_origin() {
        let mut space = DockSpace::new();
        let surface = space.add_dock("editor", DockRules::default(), vec!["one", "two"]);
        let node = NodePath::new(surface, NodeIndex::root());
        let name = detach(
            &mut space.state, &mut space.docks, &mut space.origins,
            &mut space.floating, Detach::Group(node), float_rect(), None,
            FloatHost::InWindow, &mut id,
        ).expect("valid group should detach");
        let target = space.dock_surface(&name).unwrap();
        assert_eq!(space.state[target][NodeIndex::root()].tabs_count(), 2);
        assert_eq!(space.origins.len(), 2);
        assert!(space.origins.iter().all(|origin| origin.dock == "editor"));
        assert_eq!(space.floating.pending.len(), 1);
    }
}
