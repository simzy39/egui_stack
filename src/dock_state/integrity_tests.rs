use super::*;
use std::collections::HashSet;

fn check_tree(tree: &Tree<u32>) {
    let mut reached = HashSet::new();
    let mut pending = Vec::new();
    if !tree.is_empty() {
        pending.push(NodeIndex::root());
    }
    while let Some(index) = pending.pop() {
        assert!(
            reached.insert(index),
            "cycle or repeated child at {index:?}"
        );
        let node = &tree[index];
        assert!(!node.is_empty());
        if let Some(leaf) = node.get_leaf() {
            assert!(!leaf.tabs.is_empty());
            assert!(leaf.active.0 < leaf.tabs.len());
        } else {
            assert!(node.children().len() >= 2);
            if let Some(linear) = node.get_linear() {
                assert_eq!(linear.children().len(), linear.shares().len());
                assert!(
                    linear
                        .shares()
                        .iter()
                        .all(|share| share.is_finite() && *share >= 0.0)
                );
                assert!(linear.shares().iter().sum::<f32>() > 0.0);
            }
            pending.extend(node.children());
        }
    }
    for (index, node) in tree.iter().enumerate() {
        assert_eq!(!node.is_empty(), reached.contains(&NodeIndex(index)));
    }
    if let Some(focused) = tree.focused_leaf() {
        assert!(reached.contains(&focused));
        assert!(tree[focused].is_leaf());
    }
}

#[cfg(feature = "serde")]
fn snapshot(state: &DockState<u32>) -> (serde_json::Value, Vec<u64>, Vec<String>) {
    (
        serde_json::to_value(state).unwrap(),
        state.surfaces.iter().map(|slot| slot.identity).collect(),
        state
            .iter_all_nodes()
            .map(|(_, node)| format!("{node:?}"))
            .collect(),
    )
}

/// A deterministic operation generator exercises repeated restructuring and slot
/// reuse. Its oracle checks ownership and selection independently of tree shape.
#[test]
fn generated_moves_preserve_ownership_structure_and_rejected_state() {
    for seed in 1_u64..=12 {
        let mut random = seed;
        let mut next = |limit| {
            random = random.wrapping_mul(6364136223846793005_u64).wrapping_add(1);
            (random >> 32) as usize % limit
        };
        let mut state = DockState::new((0..12).collect::<Vec<u32>>());
        state
            .main_surface_mut()
            .split_right(NodeIndex::root(), 0.5, vec![12, 13]);
        for step in 0..300 {
            let tabs: Vec<_> = state
                .iter_all_tabs()
                .map(|(path, tab)| (path, *tab))
                .collect();
            let (mut source, expected) = tabs[next(tabs.len())];
            let group = next(3) == 0;
            let selected = if group {
                let leaf = state.leaf(source.node_path()).unwrap();
                leaf.tabs[leaf.active.0]
            } else {
                expected
            };
            let target = tabs[next(tabs.len())].0.node_path();
            if step % 19 == 0 {
                state[target.surface]
                    .arrange_as_grid(NodeIndex::root(), crate::GridLayout::Columns(2));
            }
            let destination = match next(9) {
                0 => TabDestination::Node(target, TabInsert::Append),
                1 => TabDestination::Node(
                    target,
                    TabInsert::Insert(TabIndex(next(state.leaf(target).unwrap().tabs.len() + 2))),
                ),
                2 | 3 => TabDestination::Node(
                    target,
                    TabInsert::Split(
                        [Split::Left, Split::Right, Split::Above, Split::Below][next(4)],
                    ),
                ),
                4 => TabDestination::Node(
                    NodePath::new(target.surface, NodeIndex::root()),
                    TabInsert::Split(Split::Below),
                ),
                5 => TabDestination::Window(Rect::from_min_size(Pos2::ZERO, Vec2::splat(200.0))),
                6 => TabDestination::EmptySurface(SurfaceIndex(next(state.surfaces_count() + 1))),
                7 => TabDestination::Node(
                    NodePath::new(target.surface, NodeIndex(usize::MAX)),
                    TabInsert::Append,
                ),
                _ => {
                    source.tab = TabIndex(usize::MAX);
                    source.node = NodeIndex(usize::MAX);
                    TabDestination::Window(Rect::NOTHING)
                }
            };
            for index in 0..state.surfaces_count() {
                state.capture_surface_identity(SurfaceIndex(index));
            }
            #[cfg(feature = "serde")]
            let before = snapshot(&state);
            let moved = if group {
                state.move_node(source.node_path(), destination)
            } else {
                state.move_tab(source, destination)
            };
            if moved {
                let (_, tab) = state
                    .find_active_focused()
                    .expect("successful move focuses destination");
                assert_eq!(*tab, selected, "seed {seed}, step {step}");
            } else {
                #[cfg(feature = "serde")]
                assert_eq!(
                    snapshot(&state),
                    before,
                    "rejected move changed state: seed {seed}, step {step}"
                );
            }
            let mut actual: Vec<_> = state.iter_all_tabs().map(|(_, tab)| *tab).collect();
            actual.sort_unstable();
            assert_eq!(
                actual,
                (0..14).collect::<Vec<_>>(),
                "seed {seed}, step {step}"
            );
            for surface in state.iter_surfaces() {
                if let Some(tree) = surface.node_tree() {
                    check_tree(tree);
                }
            }
        }
    }
}

#[test]
fn empty_dock_and_window_trees_accept_moves_without_changing_their_host() {
    for group in [false, true] {
        for window in [false, true] {
            let mut state = DockState::new(vec![0, 1]);
            state
                .set_active_tab(TabPath::new(
                    SurfaceIndex::main(),
                    NodeIndex::root(),
                    TabIndex(1),
                ))
                .unwrap();
            let target = if window {
                state.add_window(vec![])
            } else {
                state.add_docked_surface(Tree::default())
            };
            let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
            let destination = TabDestination::EmptySurface(target);
            assert!(if group {
                state.move_node(root, destination)
            } else {
                state.move_tab((root, TabIndex(1)).into(), destination)
            });
            assert_eq!(state.is_window(target), window);
            assert_eq!(state.focused_leaf().unwrap().surface, target);
            assert_eq!(*state.find_active_focused().unwrap().1, 1);
        }
    }
}

#[test]
fn finite_corners_with_overflowing_extents_are_rejected() {
    let mut state = DockState::new(vec![0, 1]);
    let rect = Rect::from_min_max(egui::pos2(-f32::MAX, 0.0), egui::pos2(f32::MAX, 20.0));
    let root = NodePath::new(SurfaceIndex::main(), NodeIndex::root());
    assert!(!state.move_node(root, TabDestination::Window(rect)));
    assert!(state.detach_tab((root, TabIndex(0)).into(), rect).is_none());
    assert_eq!(state.surfaces_count(), 1);
    assert_eq!(state[root].tabs().unwrap(), &[0, 1]);
}

#[test]
fn filtering_preserves_the_active_member_and_chooses_a_surviving_neighbour() {
    let mut tree = Tree::new(vec![0, 1, 2, 3]);
    tree.set_active_tab(NodeIndex::root(), TabIndex(2)).unwrap();
    let mapped = tree.filter_map_tabs(|tab| (*tab != 0).then_some(*tab));
    let leaf = mapped.leaf(NodeIndex::root()).unwrap();
    assert_eq!(leaf.tabs[leaf.active.0], 2);
    tree.retain_tabs(|tab| *tab != 0);
    let leaf = tree.leaf(NodeIndex::root()).unwrap();
    assert_eq!(leaf.tabs[leaf.active.0], 2);
    tree.retain_tabs(|tab| *tab != 2);
    let leaf = tree.leaf(NodeIndex::root()).unwrap();
    assert_eq!(leaf.tabs[leaf.active.0], 1);
}

#[test]
fn removing_and_filtering_leaves_refreshes_collapse_metadata() {
    for retain in [false, true] {
        let mut tree = Tree::new(vec![0]);
        let [expanded, collapsed] = tree.split_below(NodeIndex::root(), 0.5, vec![1]);
        tree.set_leaf_collapsed(collapsed, true);
        assert!(!tree.is_collapsed());
        if retain {
            tree.retain_tabs(|tab| *tab == 1);
        } else {
            tree.remove_leaf(expanded);
        }
        assert!(tree.is_collapsed());
        assert_eq!(tree.root_node().unwrap().collapsed_leaf_count(), 1);
        tree.remove_leaf(NodeIndex::root());
        assert!(!tree.is_collapsed());
        assert!(tree.is_empty());
    }
}

#[test]
fn invalid_float_geometry_does_not_publish_docks_or_origins() {
    let mut space = crate::DockSpace::new();
    let surface = space.add_dock("main", crate::DockRules::default(), vec![0_u32, 1]);
    let path = TabPath::new(surface, NodeIndex::root(), TabIndex(0));
    for rect in [
        Rect::NOTHING,
        Rect::from_min_size(Pos2::ZERO, Vec2::ZERO),
        Rect::from_min_max(egui::pos2(-f32::MAX, 0.0), egui::pos2(f32::MAX, 10.0)),
    ] {
        assert!(
            space
                .float_out(path, rect, |tab| egui::Id::unique(*tab))
                .is_none()
        );
        assert_eq!(space.state().surfaces_count(), 1);
        assert!(space.origin(egui::Id::unique(0_u32)).is_none());
    }
    let key = space
        .float_out(
            path,
            Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.0)),
            |tab| egui::Id::unique(*tab),
        )
        .unwrap();
    assert_eq!(key, "float-1");
}

#[test]
fn window_identity_survives_tree_edits_and_retires_on_clone_and_slot_reuse() {
    let mut state = DockState::new(vec![0_u32]);
    let window = state.add_window(vec![1]);
    let identity = state
        .get_window_state_mut(window)
        .unwrap()
        .ensure_identity();
    state[window].split_right(NodeIndex::root(), 0.5, vec![2]);
    assert_eq!(
        state
            .get_window_state_mut(window)
            .unwrap()
            .ensure_identity(),
        identity
    );
    let mut cloned = state.clone();
    assert_ne!(
        cloned
            .get_window_state_mut(window)
            .unwrap()
            .ensure_identity(),
        identity
    );
    state.remove_surface(window).unwrap();
    let replacement = state.add_window(vec![3]);
    assert_eq!(replacement, window);
    assert_ne!(
        state
            .get_window_state_mut(replacement)
            .unwrap()
            .ensure_identity(),
        identity
    );
}

#[test]
fn deferred_closes_follow_surviving_leaves_when_siblings_are_promoted() {
    struct CloseAll;
    impl TabViewer for CloseAll {
        type Tab = u32;
        fn id(&mut self, tab: &mut u32) -> egui::Id {
            egui::Id::unique(*tab)
        }
        fn title(&mut self, tab: &mut u32) -> egui::WidgetText {
            tab.to_string().into()
        }
        fn ui(&mut self, _ui: &mut egui::Ui, _tab: &mut u32) {}
        fn force_close(&mut self, _tab: &mut u32) -> bool {
            true
        }
    }
    let mut state = DockState::new(vec![0, 1]);
    let [left, _] = state
        .main_surface_mut()
        .split_right(NodeIndex::root(), 0.5, vec![2, 3]);
    state.main_surface_mut().split_below(left, 0.5, vec![4, 5]);
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
        crate::DockArea::new(&mut state).show_inside(ui, &mut CloseAll);
    });
    output.textures_delta.clear();
    assert_eq!(state.iter_all_tabs().count(), 0);
    assert!(state.main_surface().is_empty());
}
