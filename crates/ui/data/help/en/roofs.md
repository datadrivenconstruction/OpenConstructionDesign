# Roofs
@tags roof, pitch, gable, dormer, extrusion
@context arch.roof arch.roof_extrusion arch.dormer arch.roof_opening
@commands arch.roof arch.roof_extrusion arch.dormer arch.roof_opening mass.roof_by_face
@see wall_profile openings_shafts ceilings

A roof is built from a footprint (slopes with a given pitch and gables) or by extruding a profile along an axis. Dormers and roof openings cut the roof by a sketch.

## steps
1. Roof by footprint: `RF` ([[cmd:arch.roof]]) - click the outline points in plan; `Enter` or a click on the first point finishes.
2. Each edge has a "sets the slope" flag: `Space` toggles it for the last edge (a gable edge has no slope). The pitch angle is in the options bar.
3. Roof by extrusion: select one straight wall and run `RX` ([[cmd:arch.roof_extrusion]]) - a 30 degree gable profile is built over the wall, as deep as the farthest parallel wall.
4. Dormer: [[cmd:arch.dormer]] - the opening outline on the slope.
5. Roof opening: [[cmd:arch.roof_opening]] - outline by points.
6. Attach the wall tops to the roof with "Attach top" ([[topic:wall_profile]]).
7. Shape of the selected roof: [[cmd:arch.roof_gable]], [[cmd:arch.roof_hip]], [[cmd:arch.roof_mansard]]; joining: [[cmd:arch.roof_join]]; edges: fascia [[cmd:arch.fascia]], gutter [[cmd:arch.gutter]], skylight [[cmd:arch.skylight]].

## keys
- `RF` - roof by footprint, `RX` - by extrusion
- `Enter` - finish the outline
- `Space` - toggle "sets the slope" on the last edge
- `Esc` - exit

## tips
- A roof on a mass face: [[cmd:mass.roof_by_face]] ([[topic:mass_by_face]]).
- The pitch angle is set in the options bar before the roof is finished.
