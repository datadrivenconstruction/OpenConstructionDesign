# Toposolid
@tags terrain, topography, surface, points, pad, contours, toposolid, site
@context site.toposolid site.edit_points
@commands site.toposolid site.toposolid_import site.boundary site.subregion site.pad site.smooth site.flatten site.edit_points site.contours site.properties
@see coordinates views_3d levels

A toposolid is the site surface built from points with elevations. The commands are on the "Architecture" tab, site group.

## steps
1. Toposolid by points: [[cmd:site.toposolid]]: click points in the plan, a number + `Enter` sets the elevation of the next points, `Enter` creates it.
2. Toposolid from a survey file: [[cmd:site.toposolid_import]]: CSV, TXT, XYZ, PTS, TSV points, or DXF and IFC.
3. Toposolid boundary: [[cmd:site.boundary]]; a sub-region with its own material: [[cmd:site.subregion]].
4. Building pad: [[cmd:site.pad]]: draw the outline, a number + `Enter` sets the elevation; cut and fill volumes show in the status bar.
5. Smooth or flatten a region: [[cmd:site.smooth]], [[cmd:site.flatten]]; single points: [[cmd:site.edit_points]] (in 3D drag the grip vertically).
6. Contour lines are shown and hidden with [[cmd:site.contours]]; site properties: [[cmd:site.properties]].

## keys
- `Enter` - finish the outline or apply
- `Backspace` - remove the last point
- `Esc` - exit

## tips
- Terrain is convenient to prepare from a DWG or PDF underlay ([[topic:import_cad]]).
- Project position relative to shared coordinates: [[topic:coordinates]].
- For large sites there should be enough points so the surface does not sag between them.
