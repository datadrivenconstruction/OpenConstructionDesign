# Wall joints (L, T, X)
@tags joints, corners, join, intersection, miter, butt
@context mod.join
@commands mod.join mod.wall_edit mod.cut_geom
@see walls wall_layers wall_profile

Where walls meet, the editor builds the joint itself: corner (L), tee (T) and cross (X), including nodes of three or more walls. Layers continue by priority, and openings and cuts are recomputed.

**Not available yet:** manual join (`JN`) and geometry cut (`mod.cut_geom`). The ribbon buttons of these commands are inactive.

## steps
1. Draw walls so their ends touch: the joint is created automatically.
2. To change the joint variant select the wall and run [[cmd:mod.wall_edit]]: a badge appears on every node.
3. Click the badge to cycle the joint: butt, miter, square-off. `Shift`+click allows or forbids joining at that node (the badge shows a cross).
4. The same panel edits layers, vertical structure and openings: [[topic:wall_layers]].
5. Joining or unjoining elements manually (`JN`, [[cmd:mod.join]]) is planned.
6. Layer cleanup follows priority: the wall with the "stronger" layer passes through the others.

## keys
- Badge click - next joint variant, `Shift`+click - allow / forbid

## tips
- Each variant switch is a separate undo step.
- A node of three or more walls is built from the actual layer bodies, not from axes.
