use egui::{Context, Id, Pos2, Rect};

use super::{
    DraggedNode, DraggedTab,
    drag_and_drop::{DragData, DragDropState, DragSource, HoverData},
};
use crate::{NodePath, Style, SurfaceIndex, TabPath};

#[derive(Clone, Copy, Debug)]
pub(super) struct TabPress {
    pub widget: Id,
    pub path: TabPath,
    pub tab_id: Id,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct GroupPress {
    pub widget: Id,
    pub source: DragSource,
}

impl GroupPress {
    /// A different handle or a replacement leaf must never inherit this gesture.
    pub(super) fn matches(self, widget: Id, path: NodePath, leaf_id: u64) -> bool {
        self.widget == widget
            && self.source == (DragSource::Group { path, id: leaf_id })
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct State {
    pub drag_start: Option<Pos2>,
    /// A tab pressed by this viewport, before or during drag activation.
    /// Kept until release, independently of the drag's activation distance.
    pub tab_press: Option<TabPress>,
    /// Retained leaf identity from pointer-down, before drag activation.
    pub group_press: Option<GroupPress>,
    /// The original source changed; suppress dock drags until the primary button is up.
    pub tab_drag_cancelled: bool,
    pub last_hover_pos: Option<Pos2>,
    pub dnd: Option<DragDropState>,
    pub window_fade: Option<(f64, SurfaceIndex)>,
    /// The tab dragged during the last frame, if any.
    pub dragged: Option<DraggedTab>,
    /// The tab group (a leaf, all its tabs) dragged during the last frame, if any.
    pub dragged_node: Option<DraggedNode>,
    /// Whether the tab gesture carries its whole group: locked at pointer-down.
    pub drag_carries_group: Option<bool>,
    /// The pass the drag was last resolved in: the docks of a space resolve it once a pass.
    pub pass: Option<u64>,
    /// Whether this area's viewport shows, as a target, the drag of a space begun in another
    /// viewport: its pointer and drag start are the session's.
    pub remote: bool,
    /// The tab whose drag was under way at the last pass: its ghost follows the pointer, so its
    /// title is left out of its bar.
    pub tab_in_air: Option<TabPath>,
    /// The tab bars drawn this pass, by leaf, in the area's viewport's points.
    pub tab_bars: Vec<(NodePath, Rect)>,
    /// The widget whose drag was cancelled because its release was lost
    /// (`DockArea::drag_lost`): egui still believes the button held and
    /// would take the drag up again, so it stays stopped until the button is let go.
    pub drag_lost: Option<Id>,
}

impl State {
    #[inline(always)]
    pub(super) fn load(ctx: &Context, id: Id) -> Self {
        ctx.data_mut(|d| d.get_temp(id)).unwrap_or(Self {
            drag_start: None,
            tab_press: None,
            group_press: None,
            tab_drag_cancelled: false,
            last_hover_pos: None,
            dnd: None,
            window_fade: None,
            dragged: None,
            dragged_node: None,
            drag_carries_group: None,
            pass: None,
            remote: false,
            tab_in_air: None,
            tab_bars: Vec::new(),
            drag_lost: None,
        })
    }

    #[inline(always)]
    pub(super) fn store(self, ctx: &Context, id: Id) {
        ctx.data_mut(|d| d.insert_temp(id, self));
    }

    /// Clear a completed or interrupted gesture, including its retained source.
    pub(super) fn finish_drag(&mut self) {
        self.reset_drag();
        self.tab_press = None;
        self.group_press = None;
    }

    pub(super) fn reset_drag(&mut self) {
        self.dnd = None;
        self.window_fade = None;
        self.drag_start = None;
        self.drag_carries_group = None;
    }

    pub(super) fn set_drag_and_drop(
        &mut self,
        drag: DragData,
        drop: HoverData,
        ctx: &Context,
        style: &Style,
    ) {
        if !self.is_drag_drop_locked(ctx, style) {
            self.dnd = Some(DragDropState {
                hover: drop,
                drag,
                pointer: ctx.pointer_hover_pos().unwrap_or(Pos2::ZERO),
                locked: None,
                src_in_window: false,
                dst_in_window: false,
            })
        }
    }

    #[inline(always)]
    fn is_drag_drop_locked(&self, ctx: &Context, style: &Style) -> bool {
        self.dnd
            .as_ref()
            .is_some_and(|drag_drop_state| drag_drop_state.is_locked(style, ctx))
    }
}

#[cfg(test)]
mod retained_source_tests {
    use super::*;

    #[test]
    fn group_press_retains_widget_and_source() {
        let source = DragSource::Group {
            path: NodePath::new(SurfaceIndex::main(), crate::NodeIndex::root()),
            id: 7,
        };
        let press = GroupPress { widget: Id::new("original"), source };
        let root = NodePath::new(SurfaceIndex::main(), crate::NodeIndex::root());
        assert!(press.matches(Id::new("original"), root, 7));
        assert!(!press.matches(Id::new("replacement"), root, 7));
        assert!(!press.matches(Id::new("original"), root, 8));
        assert!(!press.matches(Id::new("original"), NodePath::new(SurfaceIndex::main(), crate::NodeIndex(1)), 7));
    }

    #[test]
    fn finishing_a_drag_clears_both_sources_and_modifier_mode() {
        let mut state = State {
            group_press: Some(GroupPress {
                widget: Id::new("grip"),
                source: DragSource::Group {
                    path: NodePath::new(SurfaceIndex::main(), crate::NodeIndex::root()),
                    id: 42,
                },
            }),
            drag_carries_group: Some(true),
            drag_start: Some(Pos2::ZERO),
            ..State::default()
        };
        state.finish_drag();
        assert!(state.group_press.is_none());
        assert!(state.tab_press.is_none());
        assert!(state.drag_carries_group.is_none());
        assert!(state.drag_start.is_none());
    }

    #[test]
    fn finishing_a_tab_drag_clears_the_original_tab_press() {
        let mut state = State {
            tab_press: Some(TabPress {
                widget: Id::new("handle"),
                path: TabPath::new(
                    SurfaceIndex::main(),
                    crate::NodeIndex::root(),
                    crate::TabIndex(0),
                ),
                tab_id: Id::new("tab"),
            }),
            drag_carries_group: Some(false),
            ..State::default()
        };
        state.finish_drag();
        assert!(state.tab_press.is_none());
        assert!(state.group_press.is_none());
        assert!(state.drag_carries_group.is_none());
    }
}
