# Grids and reference planes
@tags grid, axes, reference plane, datum
@context arch.grid arch.grid_pick str.grid mgr.grid arch.refplane str.refplane
@commands arch.grid str.grid mgr.grid arch.refplane str.refplane str.at_grids
@see columns_beams levels snaps

Grids are the basis of coordination: columns and beams go to intersections and dimensions are taken from grid lines. Reference planes are helper lines that construction can snap to.

## steps
1. Run [[cmd:arch.grid]] (`GR`) or [[cmd:mgr.grid]] (`GD`) - pick the start and end of a grid line.
2. A grid line is a two-point tool: after the end point the next line starts again (unless "Chain" is on in the options bar). Type a length to set the end point.
3. Grid marks are named automatically: letters for horizontal lines, numbers for vertical ones; a mark is changed in the properties.
4. A helper line - [[cmd:arch.refplane]] (`RP`): walls and components can snap to it.
5. Columns at all intersections - [[cmd:str.at_grids]] ([[topic:columns_beams]]).
6. Align and lock constraints are set on a selected grid line in the constraints panel.

## keys
- `GR` / `GD` - grid line, `RP` - reference plane
- `Esc` - clear the entered points, a second `Esc` - exit

## tips
- Extents and marks of grid lines are independent per view: drag the end grips on the plan you need.
- Grids show in all plans and sections that cut through their height.
- Angular and radial dimensions from grids are in [[topic:dimensions]].
