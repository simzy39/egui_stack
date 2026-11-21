# Docking integrity

`DockState` owns the model. `DockArea` owns presentation and retained gestures;
`DockSpace` adds named docks, floating hosts, restoration origins and cross-viewport
coordination. UI previews and application drop vetoes cannot bypass model preflight.

## Moves

`move_tab`, `move_node`, `detach_tab` and float detachment share a model commit path:

1. Validate the source leaf and tab or group selection.
2. Resolve the destination and its insertion index against the original layout.
3. Detach the payload, retaining its order and selected member.
4. Insert it before removing the empty source leaf. Root splitting and subsequent
   container promotion therefore cannot change a destination before insertion.
5. Focus the destination, prune the empty source, and close an empty source window.

Rejected moves and no-ops leave the model unchanged. They do not allocate surfaces,
consume float names, update restoration origins, emit movement notifications or
unfold drop zones. Insertion indices refer to slots **before** source removal;
inserting a tab into its current slot or the next slot is a no-op. A group cannot
be moved onto itself. A group move preserves its active member; an individual tab
move activates that tab at the destination.

`TabDestination::EmptySurface` accepts a vacant slot or a tree with no tabs.
Filling an existing window preserves its window state. A new window requires
finite corners and finite, positive extents. DockSpace float geometry is checked
before model access, metadata changes or name allocation.

The commit performs no application callbacks or recoverable validation after
detachment. Allocation failure and panics in application callbacks are outside
the recoverable transaction contract. No `Tab: Clone` bound or model rollback
copy is needed.

## Identity and lifetimes

| Reference | Contract |
| --- | --- |
| `TabPath`, `NodePath`, `SurfaceIndex` | Immediate positions. Reacquire after removal or restructuring; slots can be reused. |
| `TabViewer::id` | Application identity, stable for a tab's lifetime and distinct throughout its DockSpace. Do not recycle closed tab IDs. |
| Retained tab source | Original path, logical tab ID and runtime leaf identity must all match. |
| Retained group source | Original path and runtime leaf identity must match; the group carries its current members. |
| Retained destination | Surface snapshot and node identity must match; numeric insertion also requires the same unique, ordered tab IDs. |
| Standalone window | Runtime window identity is independent of its surface slot and tree shape, and scoped to its DockArea. |

Runtime identities share one non-recycling allocator. Zero is unassigned; exhaustion
fails before wrapping. Cloning, mapping and deserialization retire old runtime
identities. Runtime tokens are excluded from persistence. Derived node rectangles
are also excluded: they are recalculated by layout, and their initial infinite
sentinels cannot be represented faithfully in JSON.

Public raw mutable model access invalidates retained surface snapshots because it
can replace components. Renderer access to geometry, selection and tab content
preserves these snapshots. Structural renderer actions use model transactions.
Applications using raw node replacement must preserve tree structure; the checked
move APIs validate endpoints rather than repair arbitrary externally corrupted
graphs. Saved trees are normalized when loaded, including obsolete active indices
and empty leaves.

## Gestures and deferred work

Pointer-down captures the originating widget, source identity and modifier mode.
A changed source cancels the gesture until release. Another handle or viewport
cannot take over the existing owner. A DockSpace has one active drag session;
remote reports expire after the current or preceding source pass, and reports
from the source viewport or a future pass are refused. Rebuilding a space discards
target reports and deferred releases. Closing a participating viewport retires or
cancels its session.

Release checks the retained source and target again before applying the model
operation. Failed commits leave origins, events and drop-zone state alone.
Deferred ejects retain the tab captured by the menu. A batch of close commands
captures identities before its first mutation and follows surviving leaves when
siblings are promoted; it cannot reinterpret an old position as a replacement tab.

## Verification

Regression tests cover stale sources and destinations, surface reuse, cloned
layouts, remote release, empty targets, windows, nested splits, focus, active
selection, geometry and deferred removals. Deterministic generated sequences run
3,600 tab/group moves across split and grid layouts and multiple surfaces. They
check tab conservation, reachability, unique parentage, valid selection and focus,
and compare serialized state, runtime tokens and complete node debug state after
rejection. These tests provide regression evidence, not a proof for every layout.
