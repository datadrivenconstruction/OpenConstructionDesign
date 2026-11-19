# Floor outline and shape
@tags outline, slope, floor shape, sketch, sub-elements
@context ctx.floor.shape ctx.floor.sketch
@commands ctx.floor.shape ctx.floor.sketch arch.floor
@see floors roofs openings_shafts

A floor has two kinds of editing: outline (boundary sketch) and shape (elevations of individual vertices and edges - this is how slopes and drains are made).

**Not available yet:** creating a new floor (`FL`) and a floor by sketch (`SB`): the commands below edit floors that already exist. The ribbon buttons of these commands are inactive.

## steps
1. Select the floor.
2. [[cmd:ctx.floor.sketch]] ("Edit boundary") edits the boundary: vertices, arcs, ellipse and spline; holes are inner loops.
3. [[cmd:ctx.floor.shape]] ("Modify sub-elements") shows grips on vertices and edges.
4. Drag a grip or type an elevation: it is measured from the slab level (`+50`, `-20`).
5. For a flat slope raise two vertices on one side; for a drain lower an inner vertex.
6. Finish with `Enter`; `Esc` cancels the current edit.

## keys
- `Enter` - apply
- `Esc` - cancel the unfinished edit
- `Tab` - next vertex or edge
- `Ctrl+Z` - undo

## tips
- The shape is computed on real solids: the slope shows in sections, quantities and IFC.
- If a sketch side does not close, the editor highlights it in red and explains why.
- Sketch-based openings are in [[topic:openings_shafts]].
