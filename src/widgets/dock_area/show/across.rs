//! The drag of a space across viewports, as each viewport's area sees it: see
//! [`session`](crate::dock_area::session).

use egui::{Id, Key, Pos2, Rect, Ui};

use super::super::{
    drag_and_drop::{DragData, DragSource, TreeComponent, make_overlay_painter},
    session::{DragSession, Outcome, RELEASED_PASSES, Report},
    state::State,
    PendingFloat,
};
use crate::dock_space::record_origin;
use crate::layout::ZoneName;
use crate::{
    DockArea, Surface, TabDestination, TabIndex, TabInsert, TabPath, TabViewer, WindowOutlinePaint,
};

/// What an area does with its space's drag this pass.
pub(super) enum Across {
    /// The drag is this viewport's, or there is none: resolved as in one viewport.
    Here,
    /// Another viewport's drag, the pointer over this one: its preview shown here, and what a
    /// release would do reported. With `take`, released where positions are hidden: the drop
    /// is this viewport's if the pointer is over one of its docks.
    Remote { take: bool },
    /// This viewport's drag, the pointer over another viewport that shows the preview.
    Covered,
    /// Settled for this pass: nothing more to resolve.
    Done,
}

impl<Tab> DockArea<'_, Tab> {
    /// Keeps the space's drag session up to date from this viewport's pass: the drag begun
    /// here (`drag_data`) moves the session's pointer, and its release applies what the viewport
    /// under the pointer reported; a drag begun elsewhere gets its pointer converted to this
    /// viewport's points, and `drag_data` set to the session's, for the preview.
    pub(super) fn across(
        &mut self,
        ui: &Ui,
        state: &mut State,
        drag_data: &mut Option<DragData>,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) -> Across {
        let session_source = self
            .space
            .as_ref()
            .and_then(|space| space.floating.session.as_ref())
            .map(|session| session.payload);
        if let Some(invalid_source) = session_source
            .filter(|source| !self.drag_source_valid(*source, tab_viewer))
        {
            // The invalid session may already have populated this viewport's preview.
            // Do not let that stale preview recreate the cancelled session below.
            if drag_data
                .as_ref()
                .is_some_and(|drag| drag.source == invalid_source)
            {
                *drag_data = None;
            }
            if let Some(space) = self.space.as_mut() {
                space.floating.session = None;
            }
            Self::idle(state);
            // The local source is validated separately before entering across(); discard
            // any preview from the invalid session even when this viewport is its target.
            state.dnd = None;
            state.window_fade = None;
            state.dragged = None;
            state.dragged_node = None;
            state.tab_in_air = None;
            self.forget_verdicts();
            ui.ctx().request_repaint();
        }
        let Some(space) = self.space.as_mut() else {
            return Across::Here;
        };
        let ctx = ui.ctx();
        let viewport = ctx.viewport_id();
        let rank = space.rank;
        let ppp = ctx.pixels_per_point();
        let (escape, released, bounds, origin, own) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.pointer.primary_released(),
                i.viewport_rect(),
                // Where the viewport is on the desktop, in physical pixels: unknown where the
                // system hides window positions.
                i.viewport().inner_rect.map(|r| r.min.to_vec2() * ppp),
                i.pointer.hover_pos(),
            )
        });

        if let Some(drag) = drag_data.as_ref() {
            // The drag is this viewport's: it keeps the pointer, even past its edges.
            let local = state
                .last_hover_pos
                .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                .unwrap_or(Pos2::ZERO);
            let others = space
                .docks
                .iter()
                .any(|dock| dock.viewport.is_some_and(|v| v != viewport));
            let session = match &mut space.floating.session {
                Some(session)
                    if session.source == viewport
                        && session.payload == drag.source
                        && session.released.is_none() =>
                {
                    session
                }
                slot => slot.insert(DragSession::new(
                    drag.source,
                    drag.rect,
                    viewport,
                    local,
                )),
            };
            session.pass += 1;
            session.local = local;
            session.desktop = origin.map(|origin| (origin + local.to_vec2() * ppp).to_pos2());
            session.outside = !bounds.contains(local);
            session.cancelled |= escape;
            if session.cancelled {
                *drag_data = None;
                state.dnd = None;
                if released {
                    space.floating.session = None;
                    state.reset_drag();
                    self.forget_verdicts();
                }
                return Across::Done;
            }
            let remote = session.remote(rank).cloned();
            if !released {
                if remote.is_some() {
                    state.dnd = None;
                    return Across::Covered;
                }
                return Across::Here;
            }
            let (source, size) = (session.payload, session.rect.size());
            // Released outside, where no viewport could tell the pointer was over it: the first
            // that sees it over one of its docks within a few passes takes it.
            let wait = remote.is_none() && session.outside && session.desktop.is_none() && others;
            if wait {
                session.released = Some(RELEASED_PASSES);
                state.reset_drag();
                state.dnd = None;
                ctx.request_repaint();
                return Across::Done;
            }
            space.floating.session = None;
            let Some(report) = remote else {
                return Across::Here;
            };
            let rect = Rect::from_min_size(local, size);
            self.release_over(report, source, rect, tab_viewer);
            state.reset_drag();
            state.dnd = None;
            self.forget_verdicts();
            ctx.request_repaint();
            return Across::Done;
        }

        let Some(session) = space.floating.session.as_mut() else {
            Self::idle(state);
            return Across::Here;
        };
        if session.source == viewport {
            // The drag begun here is no longer dragged: let go outside and waiting for another
            // viewport, or aborted (egui stops a drag on Escape).
            return match session.released {
                Some(0) => {
                    // Nobody took it: as over nothing, a new float.
                    let (source, rect) = (
                        session.payload,
                        Rect::from_min_size(session.local, session.rect.size()),
                    );
                    space.floating.session = None;
                    self.release_free(source, rect, tab_viewer);
                    self.forget_verdicts();
                    Across::Done
                }
                Some(left) => {
                    session.released = Some(left - 1);
                    ctx.request_repaint();
                    Across::Here
                }
                None => {
                    space.floating.session = None;
                    Across::Here
                }
            };
        }

        // Another viewport's drag.
        if session.cancelled {
            session.reports.remove(&viewport);
            Self::idle(state);
            return Across::Done;
        }
        let pointer = if session.released.is_some() {
            // Let go: the pointer has come into this window, or not.
            own
        } else {
            session
                .desktop
                .zip(origin)
                .map(|(desktop, origin)| ((desktop.to_vec2() - origin) / ppp).to_pos2())
        };
        let Some(pointer) = pointer.filter(|pointer| bounds.contains(*pointer)) else {
            session.reports.remove(&viewport);
            // Where positions are hidden, each window that could take the drop outlines its
            // docks once the pointer leaves the drag's window.
            let outline =
                session.desktop.is_none() && session.outside && session.released.is_none();
            let src = session.payload.component();
            Self::idle(state);
            if outline {
                self.paint_outlines(ui, &src, tab_viewer);
                ctx.request_repaint();
            }
            return Across::Done;
        };
        let (source, rect, take) = (session.payload, session.rect, session.released.is_some());
        state.remote = true;
        state.last_hover_pos = Some(pointer);
        state.drag_start.get_or_insert(pointer);
        *drag_data = Some(DragData { source, rect });
        ctx.request_repaint();
        Across::Remote { take }
    }

    /// Leaves another viewport's drag, if this area showed it.
    fn idle(state: &mut State) {
        if std::mem::take(&mut state.remote) {
            state.reset_drag();
            state.dnd = None;
        }
    }

    /// Forgets the bus's verdicts: a drag ended.
    pub(super) fn forget_verdicts(&self) {
        #[cfg(feature = "bus")]
        if let Some(bus) = self.space.as_ref().and_then(|space| space.bus) {
            bus.forget_verdicts();
        }
    }

    /// What a release over this viewport would do, for another viewport's drag: reported to
    /// the session, or, `take`n where positions are hidden, done here.
    pub(super) fn report(
        &mut self,
        ui: &Ui,
        state: &mut State,
        outcome: Outcome,
        unfold: Option<ZoneName>,
        take: bool,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) {
        let viewport = ui.ctx().viewport_id();
        let Some(space) = self.space.as_mut() else {
            return;
        };
        let rank = space.rank;
        let Some(session) = space.floating.session.as_mut() else {
            return;
        };
        if !take {
            let stamp = session.pass;
            session.reports.insert(
                viewport,
                Report {
                    stamp,
                    rank,
                    outcome,
                    unfold,
                },
            );
            return;
        }
        let (source, rect) = (session.payload, session.rect);
        match outcome {
            // Over none of its docks: another viewport may still take it.
            Outcome::Free => return,
            Outcome::Refused => space.floating.session = None,
            Outcome::Drop(_) => space.floating.session = None,
        }
        let report = Report {
            stamp: 0,
            rank,
            outcome,
            unfold,
        };
        self.release_over(report, source, rect, tab_viewer);
        Self::idle(state);
        self.forget_verdicts();
        ui.ctx().request_repaint();
    }

    /// The release of `src` over what `report` says: the move, asked to the veto again; nothing
    /// where refused; a new float at `rect` over nothing.
    fn release_over(
        &mut self,
        report: Report,
        source: DragSource,
        rect: Rect,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) {
        if !self.drag_source_valid(source, tab_viewer) {
            return;
        }
        let src = source.component();
        match report.outcome {
            Outcome::Drop(destination) => {
                if !self.destination_valid(&destination) {
                    return;
                }
                let surface = match destination {
                    TabDestination::Node(path, _) => Some(path.surface),
                    TabDestination::EmptySurface(surface) => Some(surface),
                    TabDestination::Window(_) => None,
                };
                if surface.is_none_or(|surface| {
                    self.dock_state.is_window(surface) || self.is_float_dock(surface)
                }) {
                    let may = match source {
                        DragSource::Tab { path, .. } => self.may_float(path, tab_viewer),
                        DragSource::Group { path, .. } => self.node_may_float(path, tab_viewer),
                    };
                    if !may {
                        return;
                    }
                }
                let dock = match destination {
                    TabDestination::Node(path, _) => self.dock_name(path.surface),
                    TabDestination::EmptySurface(surface) => self.dock_name(surface),
                    TabDestination::Window(_) => None,
                }
                .map(str::to_owned);
                // A stale report or a fresh veto must not change the dock's drop zone.
                if self
                    .refusal_of(&src, tab_viewer, &destination, true)
                    .is_none()
                    && self.apply_move(source, destination, tab_viewer)
                {
                    if let Some(space) = &mut self.space {
                        space.drop_zone = dock.zip(report.unfold.clone());
                        if let Some(zone) = report.unfold {
                            space.no_drop.unfolds.push(zone);
                        }
                    }
                }
            }
            Outcome::Refused => {}
            Outcome::Free => self.release_free(source, rect, tab_viewer),
        }
    }

    /// `src` released over nothing: a new float at `rect` (this viewport's points), if it may
    /// float and the veto allows it; else it stays where it was.
    fn release_free(
        &mut self,
        source: DragSource,
        rect: Rect,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) {
        if !self.floats_on() || !self.drag_source_valid(source, tab_viewer) {
            return;
        }
        let src = source.component();
        let may = match source {
            DragSource::Tab { path, .. } => self.may_float(path, tab_viewer),
            DragSource::Group { path, .. } => self.node_may_float(path, tab_viewer),
        };
        // Outside the floats' work area: refused, or torn out to a native window.
        let Some(host) = self.release_host(rect.min) else {
            return;
        };
        let destination = TabDestination::Window(rect);
        if may
            && self
                .refusal_of(&src, tab_viewer, &destination, true)
                .is_none()
        {
            self.to_float.push(PendingFloat {
                source,
                rect,
                host: Some(host),
            });
        }
    }

    /// Looks up a logical tab ID without assuming the cached path is still valid.
    pub(super) fn tab_identity(
        &mut self,
        path: TabPath,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) -> Option<Id> {
        let tab = self
            .dock_state
            .leaf_mut(path.node_path())
            .ok()?
            .tabs
            .get_mut(path.tab.0)?;
        Some(tab_viewer.id(tab))
    }

    /// Check a group against the exact leaf retained when the gesture began.
    pub(super) fn group_source_valid(&self, path: NodePath, id: u64) -> bool {
        id != 0 && self.dock_state.leaf(path).is_ok_and(|leaf| {
            !leaf.tabs.is_empty() && leaf.drag_identity == id
        })
    }

    /// A retained drag must still refer to its original tab, not just a valid slot.
    pub(super) fn drag_source_valid(
        &mut self,
        source: DragSource,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) -> bool {
        match source {
            DragSource::Tab { path, id } => self.tab_identity(path, tab_viewer) == Some(id),
            DragSource::Group { path, id } => self.group_source_valid(path, id),
        }
    }

    /// Positional existence only, for hover targets and preflight veto checks.
    /// Never use this to authorize a retained drag: use `drag_source_valid` instead.
    pub(super) fn component_exists(&self, src: &TreeComponent) -> bool {
        match *src {
            TreeComponent::Tab(path) => self
                .dock_state
                .leaf(path.node_path())
                .is_ok_and(|leaf| path.tab.0 < leaf.tabs.len()),
            TreeComponent::Node(path) => self
                .dock_state
                .leaf(path)
                .is_ok_and(|leaf| !leaf.tabs.is_empty()),
            TreeComponent::Surface(_) => false,
        }
    }

    /// Whether a destination still refers to a usable surface or leaf.
    pub(super) fn destination_valid(&self, destination: &TabDestination) -> bool {
        match *destination {
            TabDestination::Node(path, ref insert) => {
                self.dock_state.leaf(path).is_ok_and(|leaf| match insert {
                    TabInsert::Insert(index) => index.0 <= leaf.tabs.len(),
                    _ => true,
                })
            }
            TabDestination::EmptySurface(surface) => self
                .dock_state
                .get_surface(surface)
                .and_then(Surface::node_tree)
                .is_some_and(|tree| tree.is_empty()),
            TabDestination::Window(_) => true,
        }
    }

    /// Moves `src`, a tab or a whole leaf, to `destination`, once: unless the source or the
    /// destination is gone (a window closed during the drag). A tab that leaves a dock for a
    /// float's records where it came from; one that lands in a dock no float shows forgets it.
    /// Returns whether it moved.
    pub(super) fn apply_move(
        &mut self,
        source: DragSource,
        destination: TabDestination,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) -> bool {
        if !self.drag_source_valid(source, tab_viewer) {
            return false;
        }
        let src = source.component();
        if !self.destination_valid(&destination) {
            return false;
        }
        // DockState::move_node treats a group dropped on itself as a no-op.
        // Report that honestly so a rejected/no-op drop cannot unfold a zone.
        if let TabDestination::Node(target, _) = &destination {
            match source {
                DragSource::Group { path, .. } if path == *target => return false,
                DragSource::Tab { path, .. }
                    if path.node_path() == *target
                        && self.dock_state[path.node_path()].tabs_count() == 1 =>
                {
                    return false;
                }
                _ => {}
            }
        }
        let to = match destination {
            TabDestination::Node(path, _) => Some(path.surface),
            TabDestination::EmptySurface(surface) => Some(surface),
            TabDestination::Window(_) => None,
        };
        if let Some(to) = to {
            self.track_origins(&src, to, tab_viewer);
        }
        #[cfg(feature = "bus")]
        self.emit_moved(&destination);
        match src {
            TreeComponent::Node(path) => self.dock_state.move_node(path, destination),
            TreeComponent::Tab(path) => {
                let count = self.dock_state[path.node_path()].tabs_count();
                self.dock_state.move_tab(path, destination);
                // Unhide the tab bar on the source leaf when a tab is dragged out.
                if self.hidable_tab_bars
                    && let Ok(leaf) = self.dock_state.leaf_mut(path.node_path())
                    && leaf.tabs.len() < count
                    && leaf.tab_bar_hidden
                {
                    leaf.tab_bar_hidden = false;
                }
            }
            TreeComponent::Surface(_) => unreachable!("drag sources cannot be surfaces"),
        }
        true
    }

    /// The origins of the tabs of `src` moving to the surface `to`: recorded when they leave a
    /// dock for a float's, forgotten when they land in a dock no float shows, kept from float
    /// to float.
    fn track_origins(
        &mut self,
        src: &TreeComponent,
        to: crate::SurfaceIndex,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) {
        let Some(space) = self.space.as_mut() else {
            return;
        };
        if !space.floating.enabled {
            return;
        }
        let from = src.surface_address();
        let is_float = |surface| {
            space
                .docks
                .iter()
                .any(|dock| dock.surface == surface && dock.floating)
        };
        let from_float = is_float(from) || self.dock_state.is_window(from);
        let to_float = is_float(to);
        if from_float == to_float {
            return;
        }
        let paths: Vec<TabPath> = match *src {
            TreeComponent::Tab(path) => vec![path],
            TreeComponent::Node(path) => (0..self.dock_state[path].tabs_count())
                .map(|i| TabPath::new(path.surface, path.node, TabIndex(i)))
                .collect(),
            TreeComponent::Surface(_) => return,
        };
        if to_float {
            for path in paths {
                record_origin(
                    self.dock_state,
                    space.docks,
                    space.origins,
                    path,
                    &mut |tab| tab_viewer.id(tab),
                );
            }
        } else {
            for path in paths {
                if let Ok(leaf) = self.dock_state.leaf_mut(path.node_path())
                    && let Some(tab) = leaf.tabs.get_mut(path.tab.0)
                {
                    let id = tab_viewer.id(tab);
                    space.origins.retain(|origin| origin.tab != id);
                }
            }
        }
    }

    /// Outlines the docks of this viewport that could take the space's drag of `src`, where
    /// window positions are hidden.
    fn paint_outlines(
        &mut self,
        ui: &Ui,
        src: &TreeComponent,
        tab_viewer: &impl TabViewer<Tab = Tab>,
    ) {
        if !self.source_valid(src) {
            return;
        }
        let may_float = match *src {
            TreeComponent::Tab(path) => self.may_float(path, tab_viewer),
            TreeComponent::Node(path) => self.node_may_float(path, tab_viewer),
            TreeComponent::Surface(_) => false,
        };
        let Some(space) = self.space.as_ref() else {
            return;
        };
        let viewport = ui.ctx().viewport_id();
        let style = self.style.as_ref().unwrap();
        let painter = make_overlay_painter(ui);
        for dock in space.docks.iter() {
            if dock.viewport != Some(viewport) || dock.surface == src.surface_address() {
                continue;
            }
            let Some(rect) = dock.rect else {
                continue;
            };
            let refused = !dock.rules.accept_drops
                || (dock.floating && !may_float)
                || self.dock_target(&dock.name).is_none_or(|target| {
                    let destination = match target {
                        TreeComponent::Node(path) => TabDestination::Node(path, TabInsert::Append),
                        TreeComponent::Surface(surface) => TabDestination::EmptySurface(surface),
                        TreeComponent::Tab(path) => {
                            TabDestination::Node(path.node_path(), TabInsert::Append)
                        }
                    };
                    self.refusal_of(src, tab_viewer, &destination, false)
                        .is_some()
                });
            self.painter.0.window_outline(
                &painter,
                &WindowOutlinePaint {
                    rect,
                    refused,
                    style,
                    visuals: ui.visuals(),
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DockRules, DockSpace, DockState, NodeIndex, NodePath, SurfaceIndex};
    use egui::WidgetText;

    struct Viewer {
        allow_windows: bool,
    }

    impl TabViewer for Viewer {
        type Tab = &'static str;

        fn id(&mut self, tab: &mut Self::Tab) -> egui::Id {
            egui::Id::unique(*tab)
        }

        fn title(&mut self, tab: &mut Self::Tab) -> WidgetText {
            (*tab).into()
        }

        fn ui(&mut self, _ui: &mut Ui, _tab: &mut Self::Tab) {}

        fn allowed_in_windows(&self, tab: &mut Self::Tab) -> bool {
            self.allow_windows || *tab != "restricted"
        }
    }

    #[test]
    fn remote_release_rechecks_float_permission() {
        for group in [false, true] {
            for allow_windows in [false, true] {
                let mut space = DockSpace::new();
                space.add_dock("main", DockRules::default(), vec!["restricted", "allowed"]);
                let main = space.dock_surface("main").unwrap();
                let path = NodePath::new(main, NodeIndex::root());
                let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(300.0, 200.0));
                // Create a float without invoking the viewer's permission policy.
                space.add_dock("other", DockRules::default(), vec!["target"]);
                let other = space.dock_surface("other").unwrap();
                let key = space
                    .float_out(
                        TabPath::new(other, NodeIndex::root(), TabIndex(0)),
                        rect,
                        |tab| egui::Id::unique(*tab),
                    )
                    .unwrap();
                let target = space.dock_surface(&key).unwrap();
                let src = if group {
                    DragSource::Group { path, id: 0 }
                } else {
                    DragSource::Tab {
                        path: TabPath::new(main, NodeIndex::root(), TabIndex(0)),
                        id: Id::unique("restricted"),
                    }
                };
                let mut viewer = Viewer { allow_windows };
                let mut area = DockArea::dock(&mut space, "main");
                let src = if group { area.group_drag_source(path).unwrap() } else { src };
                assert!(area.is_float_dock(target));
                area.release_over(
                    Report {
                        stamp: 0,
                        rank: 0,
                        outcome: Outcome::Drop(TabDestination::Node(
                            NodePath::new(target, NodeIndex::root()),
                            TabInsert::Append,
                        )),
                        unfold: None,
                    },
                    src,
                    rect,
                    &mut viewer,
                );
                let tabs = area.dock_state[NodePath::new(target, NodeIndex::root())]
                    .tabs()
                    .unwrap();
                assert_eq!(tabs.contains(&"restricted"), allow_windows);
                assert_eq!(tabs.contains(&"allowed"), group && allow_windows);
            }
        }
    }

    #[test]
    fn group_drag_moves_current_leaf_contents_after_membership_changes() {
        let mut dock = DockState::new(vec!["original", "removed"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["destination"]);
        let source_path = NodePath::new(SurfaceIndex::main(), NodeIndex(1));
        let target_path = NodePath::new(SurfaceIndex::main(), NodeIndex(2));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = area.group_drag_source(source_path).unwrap();

        // Group identity belongs to the leaf, not a snapshot of its tabs.
        let leaf = area.dock_state.leaf_mut(source_path).unwrap();
        leaf.tabs.remove(1);
        leaf.tabs.push("added");
        assert!(area.drag_source_valid(source, &mut viewer));
        assert!(area.apply_move(
            source,
            TabDestination::Node(target_path, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(
            area.dock_state.leaf(target_path).unwrap().tabs,
            ["destination", "original", "added"],
        );
    }

    #[test]
    fn empty_group_cannot_be_moved_after_its_tabs_are_removed() {
        let mut dock = DockState::new(vec!["original"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["destination"]);
        let source_path = NodePath::new(SurfaceIndex::main(), NodeIndex(1));
        let target_path = NodePath::new(SurfaceIndex::main(), NodeIndex(2));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = area.group_drag_source(source_path).unwrap();
        area.dock_state.leaf_mut(source_path).unwrap().tabs.clear();

        assert!(!area.drag_source_valid(source, &mut viewer));
        assert!(!area.apply_move(
            source,
            TabDestination::Node(target_path, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(area.dock_state.leaf(target_path).unwrap().tabs, ["destination"]);
    }

    #[test]
    fn remote_release_cannot_move_a_replacement_group() {
        let mut dock = DockState::new(vec!["original", "other"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["destination"]);
        let source_path = NodePath::new(SurfaceIndex::main(), NodeIndex(1));
        let target_path = NodePath::new(SurfaceIndex::main(), NodeIndex(2));
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = area.group_drag_source(source_path).unwrap();

        // A replacement at the same path must not inherit the old drag identity.
        // Cloning a leaf intentionally creates a fresh runtime identity.
        let replacement = area.dock_state.leaf(source_path).unwrap().clone();
        *area.dock_state.leaf_mut(source_path).unwrap() = replacement;
        assert!(!area.drag_source_valid(source, &mut viewer));
        assert_ne!(
            area.group_drag_source(source_path),
            Some(source),
        );
        area.release_over(
            Report {
                stamp: 0,
                rank: 0,
                outcome: Outcome::Drop(TabDestination::Node(target_path, TabInsert::Append)),
                unfold: None,
            },
            source,
            rect,
            &mut viewer,
        );
        assert_eq!(area.dock_state.leaf(source_path).unwrap().tabs, ["original", "other"]);
        assert_eq!(area.dock_state.leaf(target_path).unwrap().tabs, ["destination"]);
    }

    #[test]
    fn remote_release_cannot_move_a_replacement_tab() {
        let mut dock = DockState::new(vec!["original", "other"]);
        dock.main_surface_mut().split_right(
            NodeIndex::root(),
            0.5,
            vec!["destination"],
        );
        let source_path = NodePath::new(SurfaceIndex::main(), NodeIndex(1));
        let target_path = NodePath::new(SurfaceIndex::main(), NodeIndex(2));
        let source = DragSource::Tab {
            path: TabPath::new(SurfaceIndex::main(), NodeIndex(1), TabIndex(0)),
            id: Id::unique("original"),
        };
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        area.dock_state.leaf_mut(source_path).unwrap().tabs[0] = "replacement";

        // Exercise the actual remote-release boundary, not only apply_move.
        area.release_over(
            Report {
                stamp: 0,
                rank: 0,
                outcome: Outcome::Drop(TabDestination::Node(target_path, TabInsert::Append)),
                unfold: None,
            },
            source,
            rect,
            &mut viewer,
        );
        assert_eq!(area.dock_state.leaf(source_path).unwrap().tabs, ["replacement", "other"]);
        assert_eq!(area.dock_state.leaf(target_path).unwrap().tabs, ["destination"]);
    }

    #[test]
    fn retained_tab_identity_rejects_replacement_at_same_position() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let source = DragSource::Tab {
            path: TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0)),
            id: Id::unique("first"),
        };
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        assert!(area.drag_source_valid(source, &mut viewer));
        area.dock_state.leaf_mut(root).unwrap().tabs[0] = "replacement";
        assert!(!area.drag_source_valid(source, &mut viewer));
    }

    #[test]
    fn cross_viewport_tab_session_rejects_replaced_tab_in_unchanged_slot() {
        let mut dock = DockState::new(vec!["original", "other"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let source = DragSource::Tab {
            path: TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0)),
            id: Id::unique("original"),
        };
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0));
        let session = DragSession::new(source, rect, egui::ViewportId::ROOT, Pos2::ZERO);
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        assert!(area.drag_source_valid(session.payload, &mut viewer));

        // The session still names an existing tab slot, but the tab has changed.
        area.dock_state.leaf_mut(root).unwrap().tabs[0] = "replacement";
        assert!(!area.drag_source_valid(session.payload, &mut viewer));
        assert!(!area.apply_move(
            session.payload,
            TabDestination::Node(root, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(area.dock_state.leaf(root).unwrap().tabs, ["replacement", "other"]);
    }

    #[test]
    fn retained_tab_identity_rejects_reordering_but_not_appending() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let source = DragSource::Tab {
            path: TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0)),
            id: Id::unique("first"),
        };
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        area.dock_state.leaf_mut(root).unwrap().append_tab("third");
        assert!(area.drag_source_valid(source, &mut viewer));
        area.dock_state.leaf_mut(root).unwrap().tabs.swap(0, 1);
        assert!(!area.drag_source_valid(source, &mut viewer));
    }

    #[test]
    fn group_identity_rejects_replacement_in_reused_slot() {
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut area = DockArea::new(&mut dock);
        let mut viewer = Viewer { allow_windows: true };
        let id = area.dock_state.leaf_mut(root).unwrap().ensure_drag_identity();
        let source = DragSource::Group { path: root, id };
        assert!(area.drag_source_valid(source, &mut viewer));
        area.dock_state.main_surface_mut().remove_leaf(NodeIndex::root());
        *area.dock_state.main_surface_mut() = crate::Tree::new(vec!["replacement"]);
        assert!(!area.drag_source_valid(source, &mut viewer));
    }

    #[test]
    fn stale_group_cannot_move_a_replacement_leaf() {
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = area.group_drag_source(root).unwrap();

        // The slot still exists, but it is no longer the leaf pressed by the user.
        *area.dock_state.main_surface_mut() = crate::Tree::new(vec!["replacement"]);
        let [replacement, destination] = area
            .dock_state
            .main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["destination"]);
        let target = NodePath::new(SurfaceIndex::main(), destination);
        assert!(!area.apply_move(
            source,
            TabDestination::Node(target, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(
            area.dock_state[NodePath::new(SurfaceIndex::main(), replacement)]
                .tabs()
                .unwrap(),
            &["replacement"]
        );
        assert_eq!(area.dock_state[target].tabs().unwrap(), &["destination"]);
    }

    #[test]
    fn cross_viewport_session_rejects_replaced_group_before_release() {
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = area.group_drag_source(root).unwrap();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0));
        let session = DragSession::new(source, rect, egui::ViewportId::ROOT, Pos2::ZERO);
        assert!(area.drag_source_valid(session.payload, &mut viewer));

        // A remote viewport can still hold a report, but its source is gone.
        *area.dock_state.main_surface_mut() = crate::Tree::new(vec!["replacement"]);
        assert!(!area.drag_source_valid(session.payload, &mut viewer));
        assert!(!area.apply_move(
            session.payload,
            TabDestination::Node(root, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(area.dock_state[root].tabs().unwrap(), &["replacement"]);
    }

    #[test]
    fn pointer_frames_reject_replacement_before_group_drop() {
        use egui::{Event, PointerButton, RawInput, pos2};

        let ctx = egui::Context::default();
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let source = {
            let mut area = DockArea::new(&mut dock);
            area.group_drag_source(root).unwrap()
        };
        let pointer = pos2(50.0, 40.0);
        let mut frame = |events: Vec<Event>| {
            let input = RawInput {
                events,
                ..Default::default()
            };
            let _ = ctx.run(input, |_ctx| {});
        };

        // The original press and subsequent movement occur in separate egui frames.
        frame(vec![
            Event::PointerMoved(pointer),
            Event::PointerButton {
                pos: pointer,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        assert!(ctx.input(|i| i.pointer.primary_down()));
        assert!(ctx.input(|i| i.pointer.primary_pressed()));
        frame(vec![Event::PointerMoved(pos2(100.0, 40.0))]);
        assert!(ctx.input(|i| i.pointer.primary_down()));
        // Ownership may only be captured on the first frame, not merely
        // because the pointer remains held during a later layout change.
        assert!(!ctx.input(|i| i.pointer.primary_pressed()));

        // Application code replaces the leaf while the pointer remains held.
        *dock.main_surface_mut() = crate::Tree::new(vec!["replacement"]);
        {
            let mut area = DockArea::new(&mut dock);
            assert!(!area.drag_source_valid(source, &mut viewer));
            assert!(!area.apply_move(
                source,
                TabDestination::Node(root, TabInsert::Append),
                &mut viewer,
            ));
        }
        frame(vec![Event::PointerButton {
            pos: pos2(100.0, 40.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(!ctx.input(|i| i.pointer.primary_down()));
        assert_eq!(dock[root].tabs().unwrap(), &["replacement"]);
    }

    #[test]
    fn stale_cross_viewport_group_session_does_not_poison_next_drag() {
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0));
        let mut area = DockArea::new(&mut dock);
        let original = area.group_drag_source(root).unwrap();
        let stale = DragSession::new(original, rect, egui::ViewportId::ROOT, Pos2::ZERO);
        assert!(area.drag_source_valid(stale.payload, &mut viewer));

        // A replaced leaf must invalidate the old session, but not prevent a
        // fresh gesture from starting on the new leaf at the same location.
        *area.dock_state.main_surface_mut() = crate::Tree::new(vec!["replacement"]);
        assert!(!area.drag_source_valid(stale.payload, &mut viewer));
        let fresh = area.group_drag_source(root).unwrap();
        assert_ne!(fresh, original);
        let next = DragSession::new(fresh, rect, egui::ViewportId::ROOT, Pos2::ZERO);
        assert!(area.drag_source_valid(next.payload, &mut viewer));

        let [left, right] = area
            .dock_state
            .main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["target"]);
        let left = NodePath::new(SurfaceIndex::main(), left);
        let right = NodePath::new(SurfaceIndex::main(), right);
        // Relocation invalidates even the newly retained path.
        assert!(!area.drag_source_valid(fresh, &mut viewer));
        assert!(!area.apply_move(
            fresh,
            TabDestination::Node(right, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(area.dock_state[left].tabs().unwrap(), &["replacement"]);
        assert_eq!(area.dock_state[right].tabs().unwrap(), &["target"]);
    }

    #[test]
    fn competing_group_handles_cannot_claim_the_same_press() {
        use super::super::super::state::GroupPress;
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut area = DockArea::new(&mut dock);
        let source = area.group_drag_source(root).unwrap();
        let DragSource::Group { id, .. } = source else {
            panic!("expected a group source");
        };
        let original = Id::new("original_handle");
        let competing = Id::new("competing_handle");
        let press = GroupPress { widget: original, source };
        assert!(press.matches(original, root, id));
        assert!(!press.matches(competing, root, id));
        // A newly created leaf at the same path cannot reuse the source.
        *area.dock_state.main_surface_mut() = crate::Tree::new(vec!["replacement"]);
        assert!(!area.group_source_valid(root, id));
    }

    #[test]
    fn rendered_pointer_frames_clear_stale_group_press_after_replacement() {
        use egui::{Event, PointerButton, RawInput, pos2};

        let ctx = egui::Context::default();
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let area_id = Id::new("rendered_drag_test");
        let original = {
            let mut area = DockArea::new(&mut dock);
            area.group_drag_source(root).unwrap()
        };
        let DragSource::Group { .. } = original else {
            panic!("expected group source");
        };

        let render = |events: Vec<Event>, dock: &mut DockState<&'static str>| {
            let input = RawInput {
                events,
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0))),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    DockArea::new(dock)
                        .id(area_id)
                        .show_inside(ui, &mut viewer);
                });
            });
        };
        render(vec![], &mut dock);
        let point = pos2(100.0, 30.0);
        render(vec![
            Event::PointerMoved(point),
            Event::PointerButton {
                pos: point,
                button: PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ], &mut dock);
        render(vec![Event::PointerMoved(pos2(180.0, 30.0))], &mut dock);

        // Explicitly retain the original group as the active gesture. This
        // makes the test independent of the theme's exact grip coordinates:
        // the rendered UI must actually cancel a stale retained source.
        let mut held = State::load(&ctx, area_id);
        held.group_press = Some(super::super::super::state::GroupPress {
            widget: Id::new("original_grip"),
            source: original,
        });
        held.drag_start = Some(point);
        held.store(&ctx, area_id);

        // A new leaf occupies the original location while the pointer is held.
        *dock.main_surface_mut() = crate::Tree::new(vec!["replacement"]);
        render(vec![Event::PointerMoved(pos2(210.0, 30.0))], &mut dock);
        let cancelled = State::load(&ctx, area_id);
        assert!(cancelled.tab_drag_cancelled);
        assert!(cancelled.group_press.is_none());
        assert!(cancelled.drag_start.is_none());
        render(vec![Event::PointerButton {
            pos: pos2(210.0, 30.0),
            button: PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }], &mut dock);

        assert_eq!(dock[root].tabs().unwrap(), &["replacement"]);
        let state = State::load(&ctx, area_id);
        assert!(state.group_press.is_none());
        assert!(state.tab_press.is_none());
        assert!(state.drag_carries_group.is_none());
        assert!(state.drag_start.is_none());
    }

    #[test]
    fn group_identity_rejects_a_leaf_relocated_to_another_path() {
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let id = area.dock_state.leaf_mut(root).unwrap().ensure_drag_identity();
        let source = DragSource::Group { path: root, id };
        let [old, _new] = area.dock_state.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["new"]);
        assert_ne!(old, NodeIndex::root());
        assert!(!area.drag_source_valid(source, &mut viewer));
        let relocated = NodePath::new(SurfaceIndex::main(), old);
        assert!(area.group_source_valid(relocated, id));
    }

    #[test]
    fn cloned_layout_does_not_inherit_active_group_identity() {
        let mut original = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let source = {
            let mut area = DockArea::new(&mut original);
            area.group_drag_source(root).unwrap()
        };
        let mut cloned = original.clone();
        let mut area = DockArea::new(&mut cloned);
        assert!(!area.drag_source_valid(source, &mut viewer));
        let fresh_source = area.group_drag_source(root).unwrap();
        assert!(area.drag_source_valid(fresh_source, &mut viewer));
    }

    #[test]
    fn retained_group_identity_does_not_adopt_a_new_leaf_at_same_path() {
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let original = area.group_drag_source(root).unwrap();
        *area.dock_state.leaf_mut(root).unwrap() = crate::dock_state::tree::node::LeafNode::new(vec!["replacement"]);
        let replacement = area.group_drag_source(root).unwrap();
        assert_ne!(original, replacement);
        assert!(!area.drag_source_valid(original, &mut viewer));
        assert!(area.drag_source_valid(replacement, &mut viewer));
    }

    #[test]
    fn group_identity_survives_content_changes_but_not_cloning() {
        let mut dock = DockState::new(vec!["original"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut area = DockArea::new(&mut dock);
        let mut viewer = Viewer { allow_windows: true };
        let id = area.dock_state.leaf_mut(root).unwrap().ensure_drag_identity();
        let source = DragSource::Group { path: root, id };
        area.dock_state.leaf_mut(root).unwrap().append_tab("another");
        assert!(area.drag_source_valid(source, &mut viewer));
        let cloned = area.dock_state.leaf(root).unwrap().clone();
        *area.dock_state.leaf_mut(root).unwrap() = cloned;
        assert!(!area.drag_source_valid(source, &mut viewer));
    }

    #[test]
    fn retained_group_source_rejects_empty_and_missing_leaves() {
        let mut dock = DockState::new(vec!["first"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let missing = NodePath::new(SurfaceIndex::main(), NodeIndex(100));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let id = area.dock_state.leaf_mut(root).unwrap().ensure_drag_identity();
        assert!(area.drag_source_valid(DragSource::Group { path: root, id }, &mut viewer));
        assert!(!area.drag_source_valid(DragSource::Group { path: missing, id }, &mut viewer));
        area.dock_state.leaf_mut(root).unwrap().tabs.clear();
        assert!(!area.drag_source_valid(DragSource::Group { path: root, id }, &mut viewer));
    }

    #[test]
    fn movement_rechecks_tab_identity_before_mutating_the_tree() {
        let mut dock = DockState::new(vec!["first", "second"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["target"]);
        let left = NodePath::new(SurfaceIndex::main(), NodeIndex(1));
        let right = NodePath::new(SurfaceIndex::main(), NodeIndex(2));
        let source = DragSource::Tab {
            path: TabPath::new(SurfaceIndex::main(), NodeIndex(1), TabIndex(0)),
            id: Id::unique("first"),
        };
        let destination = TabDestination::Node(right, TabInsert::Append);
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);

        // The original tab was replaced without changing the cached node or index.
        area.dock_state.leaf_mut(left).unwrap().tabs[0] = "replacement";
        assert!(!area.apply_move(source, destination, &mut viewer));
        assert_eq!(area.dock_state.leaf(left).unwrap().tabs, ["replacement", "second"]);
        assert_eq!(area.dock_state.leaf(right).unwrap().tabs, ["target"]);
    }

    #[test]
    fn movement_rechecks_group_identity_before_mutating_the_tree() {
        let mut dock = DockState::new(vec!["original"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["target"]);
        let left = NodePath::new(SurfaceIndex::main(), NodeIndex(1));
        let right = NodePath::new(SurfaceIndex::main(), NodeIndex(2));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let id = area.dock_state.leaf_mut(left).unwrap().ensure_drag_identity();
        let source = DragSource::Group { path: left, id };
        let replacement = area.dock_state.leaf(left).unwrap().clone();
        *area.dock_state.leaf_mut(left).unwrap() = replacement;
        assert!(!area.apply_move(
            source,
            TabDestination::Node(right, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(area.dock_state.leaf(left).unwrap().tabs, ["original"]);
        assert_eq!(area.dock_state.leaf(right).unwrap().tabs, ["target"]);
    }

    #[test]
    fn movement_rejects_an_invalid_destination_without_mutation() {
        let mut dock = DockState::new(vec!["first"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let source = DragSource::Tab {
            path: TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0)),
            id: Id::unique("first"),
        };
        let missing = NodePath::new(SurfaceIndex::main(), NodeIndex(100));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        assert!(!area.apply_move(
            source,
            TabDestination::Node(missing, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(area.dock_state.leaf(root).unwrap().tabs, ["first"]);
    }

    #[test]
    fn moving_a_group_onto_itself_does_not_report_success() {
        let mut dock = DockState::new(vec!["first", "second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let id = area.dock_state.leaf_mut(root).unwrap().ensure_drag_identity();
        assert!(!area.apply_move(
            DragSource::Group { path: root, id },
            TabDestination::Node(root, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(area.dock_state.leaf(root).unwrap().tabs, ["first", "second"]);
    }

    #[test]
    fn moving_a_single_tab_within_its_only_leaf_is_a_no_op() {
        let mut dock = DockState::new(vec!["only"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        assert!(!area.apply_move(
            DragSource::Tab {
                path: TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(0)),
                id: Id::unique("only"),
            },
            TabDestination::Node(root, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(area.dock_state.leaf(root).unwrap().tabs, ["only"]);
    }

    #[test]
    fn stale_drag_paths_are_rejected() {
        let mut dock = DockState::new(vec!["a"]);
        let area = DockArea::new(&mut dock);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let missing = NodePath::new(SurfaceIndex::main(), NodeIndex(100));
        assert!(area.source_valid(&TreeComponent::Node(root)));
        assert!(!area.source_valid(&TreeComponent::Node(missing)));
        assert!(!area.source_valid(&TreeComponent::Tab((root, TabIndex(1)).into())));
        assert!(
            area.destination_valid(&TabDestination::Node(root, TabInsert::Insert(TabIndex(1)),))
        );
        assert!(
            !area.destination_valid(&TabDestination::Node(root, TabInsert::Insert(TabIndex(2)),))
        );
        assert!(!area.destination_valid(&TabDestination::Node(missing, TabInsert::Append)));
        assert!(!area.destination_valid(&TabDestination::EmptySurface(SurfaceIndex::main())));
        let absent = SurfaceIndex(100);
        assert!(!area.destination_valid(&TabDestination::EmptySurface(absent)));
        assert!(!area.source_valid(&TreeComponent::Node(NodePath::new(
            absent,
            NodeIndex::root(),
        ))));
    }
}
