# Modify: move, copy, rotate and more
@tags modify, move, copy, rotate, mirror, array, align, offset, trim
@context mod.move mod.copy mod.rotate mod.scale mod.mirror mod.mirror_draw mod.array mod.offset mod.trim mod.split mod.align mod.extend mod.match mod.delete mod.cut_geom
@commands mod.move mod.copy mod.rotate mod.scale mod.mirror mod.mirror_draw mod.array mod.offset mod.trim mod.split mod.align mod.extend mod.match mod.delete
@see selection snaps undo_history clipboard

Modify commands act on the current selection: select elements first, then run the command. Joints, openings and dimensions are recomputed automatically, and the result is a single history entry.

**Not available yet:** match properties (`MA`). The ribbon buttons of these commands are inactive.

## steps
1. Run a command; it asks for the selection itself ("select elements, `Enter` - done"), or use the elements selected beforehand ([[topic:selection]]).
2. Commands: [[cmd:mod.move]] (`MV`), [[cmd:mod.copy]] (`CO`), [[cmd:mod.rotate]] (`RO`), [[cmd:mod.mirror]] (`MM`), [[cmd:mod.array]] (`AR`).
3. Pick the base point and the second point - or type a number: distance (move), angle (rotate), count (array). The options bar has "Copy", "Constrain", "Multiple", "Count", "To last", "Radial".
4. Walls: [[cmd:mod.align]] (`AL`, pick the reference line, then the wall), [[cmd:mod.offset]] (`OF`, click the side, type a distance), [[cmd:mod.trim]] (`TR`, trim to corner), [[cmd:mod.extend]] (`EX`, pick the boundary, then walls), [[cmd:mod.split]] (`SL`, click a point or type the distance from the end).
5. Confirm with `Enter` or a click. Mirror by a drawn axis - [[cmd:mod.mirror_draw]] (`DM`); the original is kept with the "Copy" option.
6. Delete the selection - [[cmd:mod.delete]] (`DE`, `Del`).

## keys
- `MV` `CO` `RO` `MM` `DM` - move, copy, rotate, mirror (pick axis / draw axis)
- `AR` - array, `OF` - offset, `AL` - align
- `TR` / `EX` / `SL` - trim, extend, split
- `Space` - in rotate: pick a new centre
- `Del` - delete

## tips
- Selected elements can also be moved by their grips without a command.
- Snaps (`Tab`) work in all modify commands: [[topic:snaps]].
