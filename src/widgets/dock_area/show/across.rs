//! The drag of a space across viewports, as each viewport's area sees it: see
//! [`session`](crate::dock_area::session).

use egui::{Id, Key, Pos2, Rect, Ui};

use super::super::{
    drag_and_drop::{DragData, DragSource, TreeComponent, make_overlay_painter},
    session::{DestinationIdentity, DragSession, Outcome, RELEASED_PASSES, Report},
    state::State,
    PendingFloat,
};
use crate::dock_space::{TabOrigin, dock_name};
use crate::layout::ZoneName;
use crate::{
    DockArea, Surface, TabDestination, TabIndex, TabInsert, TabPath, TabViewer, WindowOutlinePaint,
};

/// Origin changes staged before mutation and applied only after a successful move.
struct PendingOriginUpdate {
    entering_float: bool,
    tabs: Vec<TabOrigin>,
}

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
        let destination_identity = match &outcome {
            Outcome::Drop(TabDestination::Node(path, _)) => {
                self.capture_destination_identity(*path)
            }
            _ => None,
        };
        let insertion_order = match &outcome {
            Outcome::Drop(TabDestination::Node(path, TabInsert::Insert(_))) => self
                .dock_state
                .leaf_mut(*path)
                .ok()
                .map(|leaf| leaf.tabs.iter_mut().map(|tab| tab_viewer.id(tab)).collect()),
            _ => None,
        };
        // Capture the slot identity after all destination inspections: inspecting
        // nodes or tabs may require mutable access and invalidate a prior token.
        let destination_surface_identity = match &outcome {
            Outcome::Drop(TabDestination::Node(path, _)) => {
                self.dock_state.capture_surface_identity(path.surface)
            }
            Outcome::Drop(TabDestination::EmptySurface(surface)) => {
                self.dock_state.capture_surface_identity(*surface)
            }
            _ => None,
        };
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
                    destination_identity,
                    destination_surface_identity,
                    insertion_order,
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
            destination_identity,
            destination_surface_identity,
            insertion_order,
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
        match report.outcome.clone() {
            Outcome::Drop(destination) => {
                if !self.resolve_reported_destination(&destination, &report, tab_viewer) {
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

    /// Capture a node's stable runtime identity when a destination is reported.
    fn capture_destination_identity(
        &mut self,
        path: crate::NodePath,
    ) -> Option<DestinationIdentity> {
        let node = self.dock_state.node_mut(path).ok()?;
        if let Some(leaf) = node.get_leaf_mut() {
            return Some(DestinationIdentity::Leaf(leaf.ensure_drag_identity()));
        }
        if let Some(linear) = node.get_linear_mut() {
            return Some(if linear.dir == crate::LinearDir::Horizontal {
                DestinationIdentity::Horizontal(linear.ensure_drag_identity())
            } else {
                DestinationIdentity::Vertical(linear.ensure_drag_identity())
            });
        }
        node.get_grid_mut()
            .map(|grid| DestinationIdentity::Grid(grid.ensure_drag_identity()))
    }

    /// Read a node's identity without assigning one to a replacement.
    fn current_destination_identity(
        &self,
        path: crate::NodePath,
    ) -> Option<DestinationIdentity> {
        let node = self.dock_state.node(path).ok()?;
        if let Some(leaf) = node.get_leaf() {
            return (leaf.drag_identity != 0)
                .then_some(DestinationIdentity::Leaf(leaf.drag_identity));
        }
        if let Some(linear) = node.get_linear() {
            if linear.drag_identity == 0 {
                return None;
            }
            return Some(if linear.dir == crate::LinearDir::Horizontal {
                DestinationIdentity::Horizontal(linear.drag_identity)
            } else {
                DestinationIdentity::Vertical(linear.drag_identity)
            });
        }
        node.get_grid()
            .filter(|grid| grid.drag_identity != 0)
            .map(|grid| DestinationIdentity::Grid(grid.drag_identity))
    }

    /// Resolve the reported target as a single fail-closed validation boundary.
    ///
    /// Retained reports are untrusted snapshots: neither a still-valid index nor
    /// a still-valid node is sufficient if the enclosing surface was replaced.
    fn resolve_reported_destination(
        &mut self,
        destination: &TabDestination,
        report: &Report,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) -> bool {
        self.reported_surface_identity_valid(destination, report.destination_surface_identity)
            && self.reported_destination_identity_valid(
                destination,
                report.destination_identity.as_ref(),
            )
            && self.reported_insertion_order_valid(
                destination,
                report.insertion_order.as_deref(),
                tab_viewer,
            )
            && self.destination_valid(destination)
    }

    /// Reject retained node drops if their surface slot was recycled.
    fn reported_surface_identity_valid(
        &self,
        destination: &TabDestination,
        reported: Option<u64>,
    ) -> bool {
        match destination {
            TabDestination::Node(path, _) => reported.is_some_and(|identity| {
                self.dock_state.current_surface_identity(path.surface) == Some(identity)
            }),
            TabDestination::EmptySurface(surface) => reported.is_some_and(|identity| {
                self.dock_state.current_surface_identity(*surface) == Some(identity)
                    && self.dock_state.get_surface(*surface).is_some_and(crate::Surface::is_empty)
            }),
            TabDestination::Window(_) => reported.is_none(),
        }
    }

    /// Whether a reported destination still identifies the exact node that was hovered.
    ///
    /// A valid path is not sufficient: tree slots can be reused between the remote
    /// viewport's report and the source viewport's release.
    fn reported_destination_identity_valid(
        &self,
        destination: &TabDestination,
        identity: Option<&DestinationIdentity>,
    ) -> bool {
        match destination {
            TabDestination::Node(path, _) => identity.is_some_and(|identity| {
                self.current_destination_identity(*path).as_ref() == Some(identity)
            }),
            // Non-node destinations have no node identity to validate.
            _ => identity.is_none(),
        }
    }

    /// A positional insertion is valid only against the tab order it was previewed in.
    /// Append and split operations do not depend on an existing insertion index.
    fn reported_insertion_order_valid(
        &mut self,
        destination: &TabDestination,
        reported: Option<&[egui::Id]>,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) -> bool {
        match destination {
            TabDestination::Node(path, TabInsert::Insert(_)) => {
                let Some(reported) = reported else {
                    return false;
                };
                self.dock_state.leaf_mut(*path).is_ok_and(|leaf| {
                    let mut seen = std::collections::HashSet::with_capacity(reported.len());
                    leaf.tabs.len() == reported.len()
                        && reported.iter().all(|id| seen.insert(*id))
                        && leaf
                            .tabs
                            .iter_mut()
                            .map(|tab| tab_viewer.id(tab))
                            .eq(reported.iter().copied())
                })
            }
            _ => reported.is_none(),
        }
    }

    /// Whether a destination still refers to a usable surface or leaf.
    pub(super) fn destination_valid(&self, destination: &TabDestination) -> bool {
        self.dock_state.valid_move_destination(destination)
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
        // Use the same no-op rules as DockState, including a sole tab
        // splitting its own leaf. No-op drops must not unfold a zone.
        if let TabDestination::Node(target, _) = &destination {
            if matches!(source, DragSource::Group { path, .. } if path == *target) {
                return false;
            }
        }
        if let DragSource::Tab { path, .. } = source {
            if self.dock_state.tab_move_is_noop(path, &destination) {
                return false;
            }
        }
        let to = match destination {
            TabDestination::Node(path, _) => Some(path.surface),
            TabDestination::EmptySurface(surface) => Some(surface),
            TabDestination::Window(_) => None,
        };
        let origin_update = to.and_then(|surface| {
            self.prepare_origin_update(&src, surface, tab_viewer)
        });
        match src {
            TreeComponent::Node(path) => {
                if !self.dock_state.move_node(path, destination) {
                    return false;
                }
            },
            TreeComponent::Tab(path) => {
                let count = self.dock_state[path.node_path()].tabs_count();
                if !self.dock_state.move_tab(path, destination) {
                    return false;
                }
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
        self.commit_origin_update(origin_update);
        // Notify observers only after DockState confirms that the move occurred.
        #[cfg(feature = "bus")]
        self.emit_moved(&destination);
        true
    }

    /// The origins of the tabs of `src` moving to the surface `to`: recorded when they leave a
    /// dock for a float's, forgotten when they land in a dock no float shows, kept from float
    /// to float.
    /// Capture source identities and neighbours before moving, without changing origins.
    /// The returned update is committed only if the dock mutation succeeds.
    fn prepare_origin_update(
        &mut self,
        src: &TreeComponent,
        to: crate::SurfaceIndex,
        tab_viewer: &mut impl TabViewer<Tab = Tab>,
    ) -> Option<PendingOriginUpdate> {
        let space = self.space.as_ref()?;
        if !space.floating.enabled {
            return None;
        }
        let from = src.surface_address();
        let is_float = |surface| {
            space.docks.iter().any(|dock| dock.surface == surface && dock.floating)
        };
        let from_float = is_float(from) || self.dock_state.is_window(from);
        let to_float = is_float(to);
        if from_float == to_float {
            return None;
        }
        let dock = if to_float {
            Some(dock_name(space.docks, from)?.to_owned())
        } else {
            None
        };
        let paths: Vec<TabPath> = match *src {
            TreeComponent::Tab(path) => vec![path],
            TreeComponent::Node(path) => (0..self.dock_state[path].tabs_count())
                .map(|i| TabPath::new(path.surface, path.node, TabIndex(i)))
                .collect(),
            TreeComponent::Surface(_) => return None,
        };
        let mut updates = Vec::with_capacity(paths.len());
        for path in paths {
            let leaf = self.dock_state.leaf_mut(path.node_path()).ok()?;
            let at = path.tab.0;
            let tabs = leaf.tabs_mut();
            if at >= tabs.len() {
                return None;
            }
            let tab = tab_viewer.id(&mut tabs[at]);
            let before = at.checked_sub(1).map(|i| tab_viewer.id(&mut tabs[i]));
            let after = tabs.get_mut(at + 1).map(|next| tab_viewer.id(next));
            updates.push(TabOrigin::new(
                tab,
                dock.as_deref().unwrap_or_default(),
                before,
                after,
            ));
        }
        Some(PendingOriginUpdate {
            entering_float: to_float,
            tabs: updates,
        })
    }

    fn commit_origin_update(&mut self, update: Option<PendingOriginUpdate>) {
        let Some(update) = update else { return };
        let Some(space) = self.space.as_mut() else { return };
        for origin in update.tabs {
            space.origins.retain(|existing| existing.tab != origin.tab);
            if update.entering_float {
                space.origins.push(origin);
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
    fn retained_empty_surface_drop_rejects_reused_slot() {
        let mut dock = DockState::new(vec!["source"]);
        let vacant = dock.add_window(vec!["old"]);
        let untouched = dock.add_window(vec!["other"]);
        dock.remove_surface(vacant).unwrap();
        assert!(dock.is_surface_valid(untouched));
        let mut area = DockArea::new(&mut dock);
        let original = area.dock_state.capture_surface_identity(vacant).unwrap();
        let destination = TabDestination::EmptySurface(vacant);
        assert!(area.reported_surface_identity_valid(&destination, Some(original)));

        let occupied = area.dock_state.add_window(vec!["replacement"]);
        assert_eq!(occupied, vacant);
        area.dock_state.remove_surface(occupied).unwrap();
        assert!(area.dock_state.get_surface(vacant).is_none_or(crate::Surface::is_empty));
        assert!(!area.reported_surface_identity_valid(&destination, Some(original)));
    }

    #[test]
    fn retained_drop_rejects_a_recycled_destination_surface() {
        let mut dock = DockState::new(vec!["source"]);
        let old = dock.add_window(vec!["old target"]);
        let mut area = DockArea::new(&mut dock);
        let original = area.dock_state.capture_surface_identity(old).unwrap();
        assert!(area.reported_surface_identity_valid(
            &TabDestination::Node(
                NodePath::new(old, NodeIndex::root()),
                TabInsert::Append,
            ),
            Some(original),
        ));

        assert!(area.dock_state.remove_surface(old).is_some());
        let replacement = area.dock_state.add_window(vec!["replacement"]);
        assert_eq!(old, replacement);
        let new_identity = area.dock_state.capture_surface_identity(replacement).unwrap();
        assert_ne!(original, new_identity);
        assert!(!area.reported_surface_identity_valid(
            &TabDestination::Node(
                NodePath::new(replacement, NodeIndex::root()),
                TabInsert::Append,
            ),
            Some(original),
        ));
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
                let destination_identity = area
                    .capture_destination_identity(NodePath::new(target, NodeIndex::root()));
        let surface_identity_0 = area.dock_state.capture_surface_identity(target);
                area.release_over(
                    Report {
                        stamp: 0,
                        rank: 0,
                        outcome: Outcome::Drop(TabDestination::Node(
                            NodePath::new(target, NodeIndex::root()),
                            TabInsert::Append,
                        )),
                        destination_identity,
                        destination_surface_identity: surface_identity_0,
                        insertion_order: None,
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
    fn origin_records_change_only_after_successful_move() {
        let mut space = DockSpace::new();
        space.add_dock("main", DockRules::default(), vec!["first", "second"]);
        space.add_dock("other", DockRules::default(), vec!["target"]);
        let main = space.dock_surface("main").unwrap();
        let other = space.dock_surface("other").unwrap();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(300.0, 200.0));
        let key = space.float_out(
            TabPath::new(other, NodeIndex::root(), TabIndex(0)),
            rect,
            |tab| Id::unique(*tab),
        ).unwrap();
        let float_surface = space.dock_surface(&key).unwrap();
        let path = TabPath::new(main, NodeIndex::root(), TabIndex(0));
        let mut viewer = Viewer { allow_windows: true };
        {
            let mut area = DockArea::dock(&mut space, "main");
            let source = DragSource::Tab {
                path,
                id: area.tab_identity(path, &mut viewer).unwrap(),
            };
            assert!(!area.apply_move(
                source,
                TabDestination::Node(
                    NodePath::new(float_surface, NodeIndex(999)),
                    TabInsert::Append,
                ),
                &mut viewer,
            ));
        }
        assert!(space.origin(Id::unique("first")).is_none());
        {
            let mut area = DockArea::dock(&mut space, "main");
            let source = DragSource::Tab {
                path,
                id: area.tab_identity(path, &mut viewer).unwrap(),
            };
            assert!(area.apply_move(
                source,
                TabDestination::Node(
                    NodePath::new(float_surface, NodeIndex::root()),
                    TabInsert::Append,
                ),
                &mut viewer,
            ));
        }
        let origin = space.origin(Id::unique("first")).expect("successful move records origin");
        assert_eq!(origin.dock, "main");
        assert_eq!(origin.after, Some(Id::unique("second")));

        // A rejected return must not erase the saved origin.
        let float_path = TabPath::new(float_surface, NodeIndex::root(), TabIndex(1));
        {
            let mut area = DockArea::dock(&mut space, "main");
            let source = DragSource::Tab {
                path: float_path,
                id: area.tab_identity(float_path, &mut viewer).unwrap(),
            };
            assert!(!area.apply_move(
                source,
                TabDestination::Node(
                    NodePath::new(main, NodeIndex(999)),
                    TabInsert::Append,
                ),
                &mut viewer,
            ));
        }
        assert!(space.origin(Id::unique("first")).is_some());

        // Returning the tab to its original dock removes its restoration record.
        {
            let mut area = DockArea::dock(&mut space, "main");
            let source = DragSource::Tab {
                path: float_path,
                id: area.tab_identity(float_path, &mut viewer).unwrap(),
            };
            assert!(area.apply_move(
                source,
                TabDestination::Node(
                    NodePath::new(main, NodeIndex::root()),
                    TabInsert::Append,
                ),
                &mut viewer,
            ));
        }
        assert!(space.origin(Id::unique("first")).is_none());
    }

    #[test]
    fn group_move_to_float_records_origins_only_after_success() {
        let mut space = DockSpace::new();
        space.add_dock("main", DockRules::default(), vec!["first", "second"]);
        space.add_dock("other", DockRules::default(), vec!["target"]);
        let main = space.dock_surface("main").unwrap();
        let other = space.dock_surface("other").unwrap();
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(300.0, 200.0));
        let key = space.float_out(
            TabPath::new(other, NodeIndex::root(), TabIndex(0)),
            rect,
            |tab| Id::unique(*tab),
        ).unwrap();
        let target = space.dock_surface(&key).unwrap();
        let source_path = NodePath::new(main, NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        {
            let mut area = DockArea::dock(&mut space, "main");
            let source = area.group_drag_source(source_path).unwrap();
            assert!(!area.apply_move(
                source,
                TabDestination::Node(NodePath::new(target, NodeIndex(999)), TabInsert::Append),
                &mut viewer,
            ));
        }
        assert!(space.origin(Id::unique("first")).is_none());
        assert!(space.origin(Id::unique("second")).is_none());
        {
            let mut area = DockArea::dock(&mut space, "main");
            let source = area.group_drag_source(source_path).unwrap();
            assert!(area.apply_move(
                source,
                TabDestination::Node(NodePath::new(target, NodeIndex::root()), TabInsert::Append),
                &mut viewer,
            ));
        }
        for tab in ["first", "second"] {
            assert_eq!(space.origin(Id::unique(tab)).unwrap().dock, "main");
        }
    }

    #[test]
    fn destination_validation_accepts_vacant_surface_and_rejects_occupied_surface() {
        let mut dock = DockState::new(vec!["first"]);
        let mut area = DockArea::new(&mut dock);
        assert!(!area.destination_valid(&TabDestination::EmptySurface(SurfaceIndex::main())));
        assert!(!area.destination_valid(&TabDestination::EmptySurface(SurfaceIndex(999))));
    }

    #[test]
    fn positional_insert_report_rejects_reordered_destination_tabs() {
        let mut dock = DockState::new(vec!["first", "second", "third"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let destination = TabDestination::Node(root, TabInsert::Insert(TabIndex(1)));
        let reported: Vec<_> = area
            .dock_state
            .leaf_mut(root)
            .unwrap()
            .tabs
            .iter_mut()
            .map(|tab| viewer.id(tab))
            .collect();

        assert!(area.reported_insertion_order_valid(
            &destination,
            Some(&reported),
            &mut viewer,
        ));
        area.dock_state.leaf_mut(root).unwrap().tabs.swap(0, 2);
        assert!(!area.reported_insertion_order_valid(
            &destination,
            Some(&reported),
            &mut viewer,
        ));
        assert!(!area.reported_insertion_order_valid(
            &destination,
            None,
            &mut viewer,
        ));
    }

    #[test]
    fn positional_insert_report_rejects_ambiguous_tab_identities() {
        let mut dock = DockState::new(vec!["duplicate", "duplicate"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let destination = TabDestination::Node(root, TabInsert::Insert(TabIndex(1)));
        let reported: Vec<_> = area
            .dock_state
            .leaf_mut(root)
            .unwrap()
            .tabs
            .iter_mut()
            .map(|tab| viewer.id(tab))
            .collect();

        assert_eq!(reported[0], reported[1]);
        assert!(!area.reported_insertion_order_valid(
            &destination,
            Some(&reported),
            &mut viewer,
        ));
    }

    #[test]
    fn remote_release_rejects_destination_insert_index_stale_after_tab_removal() {
        let mut dock = DockState::new(vec!["source"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["first", "second"]);
        let source_path = NodePath::new(SurfaceIndex::main(), NodeIndex(1));
        let target_path = NodePath::new(SurfaceIndex::main(), NodeIndex(2));
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source_path_tab = TabPath::new(SurfaceIndex::main(), NodeIndex(1), TabIndex(0));
        let source = DragSource::Tab {
            path: source_path_tab,
            id: area.tab_identity(source_path_tab, &mut viewer).unwrap(),
        };
        assert!(area.drag_source_valid(source, &mut viewer));

        // The preview allowed insertion at index two, but a tab was removed
        // before the receiving viewport processed the release.
        area.dock_state.leaf_mut(target_path).unwrap().tabs.pop();
        let destination = TabDestination::Node(target_path, TabInsert::Insert(TabIndex(2)));
        assert!(!area.destination_valid(&destination));
        let destination_identity = area.capture_destination_identity(target_path);
        let surface_identity_1 = area.dock_state.capture_surface_identity(target_path.surface);
        area.release_over(
            Report {
                stamp: 0,
                rank: 0,
                outcome: Outcome::Drop(destination),
                destination_identity,
                destination_surface_identity: surface_identity_1,
                insertion_order: None,
                unfold: None,
            },
            source,
            rect,
            &mut viewer,
        );
        assert_eq!(area.dock_state.leaf(source_path).unwrap().tabs, ["source"]);
        assert_eq!(area.dock_state.leaf(target_path).unwrap().tabs, ["first"]);
    }

    #[test]
    fn remote_release_cannot_target_a_replacement_leaf_at_the_same_path() {
        let mut dock = DockState::new(vec!["source"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["original target"]);
        let source_path = NodePath::new(SurfaceIndex::main(), NodeIndex(1));
        let target_path = NodePath::new(SurfaceIndex::main(), NodeIndex(2));
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = area.group_drag_source(source_path).unwrap();
        let target_identity = area.capture_destination_identity(target_path).unwrap();

        // Replacing the leaf preserves its positional path, but must not inherit
        // the identity captured by the other viewport's drop report.
        let replacement = area.dock_state.leaf(target_path).unwrap().clone();
        *area.dock_state.leaf_mut(target_path).unwrap() = replacement;

        let surface_identity_2 = area.dock_state.capture_surface_identity(target_path.surface);
        area.release_over(
            Report {
                stamp: 0,
                rank: 0,
                outcome: Outcome::Drop(TabDestination::Node(
                    target_path,
                    TabInsert::Append,
                )),
                destination_identity: Some(target_identity),
                destination_surface_identity: surface_identity_2,
                insertion_order: None,
                unfold: None,
            },
            source,
            rect,
            &mut viewer,
        );

        assert_eq!(area.dock_state.leaf(source_path).unwrap().tabs, ["source"]);
        assert_eq!(
            area.dock_state.leaf(target_path).unwrap().tabs,
            ["original target"],
        );
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
    fn unchanged_tab_reorder_does_not_report_a_move() {
        let mut dock = DockState::new(vec!["first", "second", "third"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = DragSource::Tab {
            path: TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(1)),
            id: Id::unique("second"),
        };
        for insert in [
            TabInsert::Insert(TabIndex(1)),
            TabInsert::Insert(TabIndex(2)),
        ] {
            assert!(!area.apply_move(
                source,
                TabDestination::Node(root, insert),
                &mut viewer,
            ));
            assert_eq!(
                area.dock_state.leaf(root).unwrap().tabs,
                ["first", "second", "third"],
            );
        }
        let last = DragSource::Tab {
            path: TabPath::new(SurfaceIndex::main(), NodeIndex::root(), TabIndex(2)),
            id: Id::unique("third"),
        };
        assert!(!area.apply_move(
            last,
            TabDestination::Node(root, TabInsert::Append),
            &mut viewer,
        ));
    }

    #[test]
    fn moved_group_cannot_reuse_its_source_identity() {
        let mut dock = DockState::new(vec!["original", "other"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["destination"]);
        let source_path = NodePath::new(SurfaceIndex::main(), NodeIndex(1));
        let target_path = NodePath::new(SurfaceIndex::main(), NodeIndex(2));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = area.group_drag_source(source_path).unwrap();

        assert!(area.apply_move(
            source,
            TabDestination::Node(target_path, TabInsert::Append),
            &mut viewer,
        ));
        assert!(!area.drag_source_valid(source, &mut viewer));
        assert!(!area.apply_move(
            source,
            TabDestination::Node(target_path, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(
            area.dock_state.leaf(target_path).unwrap().tabs,
            ["destination", "original", "other"],
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
        let destination_identity = area.capture_destination_identity(target_path);
        let surface_identity_3 = area.dock_state.capture_surface_identity(target_path.surface);
        area.release_over(
            Report {
                stamp: 0,
                rank: 0,
                outcome: Outcome::Drop(TabDestination::Node(target_path, TabInsert::Append)),
                destination_identity,
                destination_surface_identity: surface_identity_3,
                insertion_order: None,
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
        let destination_identity = area.capture_destination_identity(target_path);
        let surface_identity_4 = area.dock_state.capture_surface_identity(target_path.surface);
        area.release_over(
            Report {
                stamp: 0,
                rank: 0,
                outcome: Outcome::Drop(TabDestination::Node(target_path, TabInsert::Append)),
                destination_identity,
                destination_surface_identity: surface_identity_4,
                insertion_order: None,
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
    fn remote_release_moves_retained_tab_to_another_leaf() {
        let mut dock = DockState::new(vec!["source", "remaining"]);
        let [left, right] = dock.main_surface_mut().split_right(
            NodeIndex::root(), 0.5, vec!["target"],
        );
        let source_path = TabPath::new(SurfaceIndex::main(), left, TabIndex(0));
        let destination_path = NodePath::new(SurfaceIndex::main(), right);
        let rect = Rect::from_min_size(Pos2::ZERO, egui::vec2(200.0, 100.0));
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = DragSource::Tab {
            path: source_path,
            id: area.tab_identity(source_path, &mut viewer).unwrap(),
        };
        let session = DragSession::new(source, rect, egui::ViewportId::ROOT, Pos2::ZERO);
        assert!(area.drag_source_valid(session.payload, &mut viewer));
        let destination_identity = area.capture_destination_identity(destination_path);
        let surface_identity_5 = area.dock_state.capture_surface_identity(destination_path.surface);
        area.release_over(
            Report {
                stamp: 0,
                rank: 0,
                outcome: Outcome::Drop(TabDestination::Node(
                    destination_path,
                    TabInsert::Append,
                )),
                destination_identity,
                destination_surface_identity: surface_identity_5,
                insertion_order: None,
                unfold: None,
            },
            session.payload,
            rect,
            &mut viewer,
        );
        assert_eq!(area.dock_state.leaf(source_path.node_path()).unwrap().tabs, ["remaining"]);
        assert_eq!(area.dock_state.leaf(destination_path).unwrap().tabs, ["target", "source"]);
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
    fn group_drag_rejects_reused_leaf_index_after_tree_restructure() {
        let mut dock = DockState::new(vec!["original"]);
        let [left, right] = dock.main_surface_mut().split_right(
            NodeIndex::root(), 0.5, vec!["other"],
        );
        let source_path = NodePath::new(SurfaceIndex::main(), left);
        let destination = NodePath::new(SurfaceIndex::main(), right);
        let mut viewer = Viewer { allow_windows: true };
        let mut area = DockArea::new(&mut dock);
        let source = area.group_drag_source(source_path).unwrap();
        assert!(area.drag_source_valid(source, &mut viewer));
        // Rebuilding a tree may reuse exactly the same node index. A valid
        // path alone must never authorize the old drag against the new leaf.
        *area.dock_state.main_surface_mut() = crate::Tree::new(vec!["replacement"]);
        let [new_left, new_right] = area.dock_state.main_surface_mut().split_right(
            NodeIndex::root(), 0.5, vec!["new target"],
        );
        assert_eq!(left, new_left);
        assert_eq!(right, new_right);
        assert!(!area.drag_source_valid(source, &mut viewer));
        assert!(!area.apply_move(
            source,
            TabDestination::Node(destination, TabInsert::Append),
            &mut viewer,
        ));
        assert_eq!(area.dock_state[source_path].tabs().unwrap(), &["replacement"]);
        assert_eq!(area.dock_state[destination].tabs().unwrap(), &["new target"]);
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
        *area.dock_state.leaf_mut(root).unwrap() = crate::LeafNode::new(vec!["replacement"]);
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
    fn reported_leaf_destination_rejects_replacement_at_the_same_path() {
        let mut dock = DockState::new(vec!["source", "original target"]);
        let destination_path = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut area = DockArea::new(&mut dock);
        let destination_identity = area.capture_destination_identity(destination_path).unwrap();

        for insert in [
            TabInsert::Append,
            TabInsert::Insert(TabIndex(0)),
            TabInsert::Split(crate::Split::Right),
        ] {
            assert!(area.reported_destination_identity_valid(
                &TabDestination::Node(destination_path, insert),
                Some(&destination_identity),
            ));
        }

        *area.dock_state.main_surface_mut() = crate::Tree::new(vec!["replacement target"]);

        for insert in [
            TabInsert::Append,
            TabInsert::Insert(TabIndex(0)),
            TabInsert::Split(crate::Split::Right),
        ] {
            assert!(!area.reported_destination_identity_valid(
                &TabDestination::Node(destination_path, insert),
                Some(&destination_identity),
            ));
        }
        assert!(!area.reported_destination_identity_valid(
            &TabDestination::Node(destination_path, TabInsert::Append),
            None,
        ));
    }

    #[test]
    fn node_destination_without_reported_identity_fails_closed() {
        let mut dock = DockState::new(vec!["target"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let area = DockArea::new(&mut dock);
        assert!(!area.reported_destination_identity_valid(
            &TabDestination::Node(root, TabInsert::Append),
            None,
        ));
    }

    #[test]
    fn reported_container_destination_rejects_replacement_at_the_same_path() {
        let mut dock = DockState::new(vec!["first"]);
        dock.main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec!["second"]);
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut area = DockArea::new(&mut dock);
        let identity = area.capture_destination_identity(root).unwrap();

        assert!(area.reported_destination_identity_valid(
            &TabDestination::Node(root, TabInsert::Split(crate::Split::Below)),
            Some(&identity),
        ));

        let replacement = match area.dock_state.node(root).unwrap() {
            crate::Node::Linear(linear) => linear.clone(),
            _ => panic!("expected a linear root"),
        };
        *area.dock_state.node_mut(root).unwrap() = crate::Node::Linear(replacement);

        assert!(!area.reported_destination_identity_valid(
            &TabDestination::Node(root, TabInsert::Split(crate::Split::Below)),
            Some(&identity),
        ));
    }

    #[test]
    fn reported_grid_destination_rejects_replacement_at_the_same_path() {
        let mut dock = DockState::new(vec!["one", "two", "three", "four"]);
        assert!(dock
            .main_surface_mut()
            .arrange_as_grid(NodeIndex::root(), crate::GridLayout::Auto));
        let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
        let mut area = DockArea::new(&mut dock);
        let identity = area.capture_destination_identity(root).unwrap();

        assert!(area.reported_destination_identity_valid(
            &TabDestination::Node(root, TabInsert::Split(crate::Split::Below)),
            Some(&identity),
        ));

        let replacement = area
            .dock_state
            .node(root)
            .unwrap()
            .get_grid()
            .unwrap()
            .clone();
        *area.dock_state.node_mut(root).unwrap() = crate::Node::Grid(replacement);

        assert!(!area.reported_destination_identity_valid(
            &TabDestination::Node(root, TabInsert::Split(crate::Split::Below)),
            Some(&identity),
        ));
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
