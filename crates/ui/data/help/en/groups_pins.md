# Groups and pinning
@tags group, pin, lock
@context mod.group mod.ungroup mod.pin mod.unpin
@commands mod.group mod.ungroup mod.pin mod.unpin
@see modify selection

A group joins elements so they move and copy together. Pinning protects an element from accidental moves.

**Status: planned.** Groups and pinning is not available in this build yet: the ribbon button is inactive. The steps below describe the intended workflow.

## steps
1. Select elements and press `Ctrl+G` ([[cmd:mod.group]]) to make a group.
2. A group moves and copies as a unit (`MV`, `CO`).
3. Ungroup - `Ctrl+Shift+G` ([[cmd:mod.ungroup]]).
4. Pin - `PN` ([[cmd:mod.pin]]): the element cannot be moved or deleted by accident.
5. Unpin - `UP` ([[cmd:mod.unpin]]).
6. Pinned items are visible in the selection filter: `SX` ([[topic:selection]]).

## keys
- `Ctrl+G` - group, `Ctrl+Shift+G` - ungroup
- `PN` - pin, `UP` - unpin
- `Ctrl+Z` - undo

## tips
- Pin grids, levels and underlays right after setting them up.
- A group suits typical details repeated on storeys.
- Pinning does not stop you from changing element parameters.
