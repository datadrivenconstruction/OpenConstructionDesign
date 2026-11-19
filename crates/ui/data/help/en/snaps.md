# Snaps and the work plane
@tags snap, ortho, work plane, osnap
@context mgr.snaps mgr.workplane
@commands mgr.snaps mgr.workplane
@see navigation_2d modify walls

Snaps pull the cursor to characteristic points: endpoints, midpoints, intersections, grids. The work plane defines where points are placed in 3D.

**Not available yet:** the snap toggle in the status bar, `F3` and the `Shift` / `Alt` modifiers. The ribbon buttons of these commands are inactive.

## steps
1. Snaps are always on: endpoint, midpoint, intersection, perpendicular, wall axis and face, grid axis, reference plane, nearest point on an underlay line, angles in steps of 45 degrees from the last point, the grid.
2. While drawing, a snap glyph and label appear next to the cursor; endpoints and intersections take priority over the grid.
3. `Tab` cycles candidates (axis, inner face, outer face of a wall).
4. `WP` ([[cmd:mgr.workplane]]) shows where 3D points go now and moves the work plane: onto a level with an offset, onto a reference plane, onto a face you pick or through three points. "Back to the active level" restores the default.

## keys
- `Tab` - next candidate
- `WP` - work plane

## tips
- Snapping also works to lines of underlays.
- Zoom affects the snap radius: zoom in to pick more precisely.
- A typed number always beats a snap.
