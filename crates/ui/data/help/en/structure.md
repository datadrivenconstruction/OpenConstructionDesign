# Structural elements
@tags footing, strip footing, raft, brace, rebar, beam system, structure
@context str.footing str.strip_footing str.raft str.brace str.beam_system str.rebar str.analytical
@commands str.footing str.strip_footing str.raft str.brace str.beam_system str.rebar str.analytical str.wall str.slab
@see columns_beams grids floors

The "Structure" tab collects load-bearing elements: walls and slabs, columns and beams, foundations, bracing, reinforcement and the analytical model.

**Not available yet:** the structural floor (`SB`); its ribbon button is inactive.

## steps
1. Available now: bearing wall `SW` ([[cmd:str.wall]]), column and beam ([[topic:columns_beams]]), grids ([[topic:grids]]), slab opening `SO` ([[topic:openings_shafts]]).
2. Column footing `FT` ([[cmd:str.footing]]): click a column or a point, `Space` rotates; strip footing `SF` ([[cmd:str.strip_footing]]) along a wall or by two points; foundation slab `FR` ([[cmd:str.raft]]) by outline. Under every column at once: [[cmd:str.footing_all]].
3. Beam system `BS` ([[cmd:str.beam_system]]) by outline, `Space` sets the beam direction; brace `BR` ([[cmd:str.brace]]) and truss ([[cmd:str.truss]]) by two points. A diagrid, gridshell or cable net ([[cmd:str.gridshell]]) takes two plan corners: a dome on the inscribed circle, a barrel vault along the long side, a saddle or a flat grid; the options set the rise, divisions, pattern and sections.
4. Reinforcement `RB` ([[cmd:str.rebar]]): select columns, beams, walls, slabs or footings (plan or 3D) and choose longitudinal bars (bottom, top, sides, corners, perimeter; straight, L or U, hooks 90/135/180°), stirrups (fixed count, maximum spacing, spacing, denser end zones) or a two-layer mesh (spacing and direction per face); the Cover tab sets the cover per face. Bars follow every host edit and are clipped to it. The bending schedule ([[cmd:str.rebar_schedule]]) lists marks with length including hooks and weight, exports CSV and is also a schedule tab; [[cmd:str.rebar_delete]] removes the selected bar sets or all bars of the selected hosts.
5. The analytical model `AN` ([[cmd:str.analytical]]) is derived from structural columns, beams, braces, walls and slabs: nodes within 5 cm merge, members are split at nodes, footings give supports. The window lists issues (free ends, close nodes, short members, unsupported parts), sets fixed/pinned end releases and end offsets for the selected element and exports JSON ([[cmd:str.analytical_export]]). IFC export writes bars and the structural analysis model.

## keys
- `SW` - bearing wall, `SO` - slab opening
- `Tab` - snap, `Esc` - exit

## tips
- Columns and beams: [[topic:columns_beams]]; the grid is the basis for snapping: [[topic:grids]].
- Bearing elements take part in clash detection ([[topic:clash]]).
