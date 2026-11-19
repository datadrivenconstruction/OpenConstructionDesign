# Columns and beams
@tags column, beam, structural, member
@context arch.column str.column str.beam struct.beam_pick str.at_grids
@commands arch.column str.column str.beam str.at_grids str.beam_system
@see grids structure

Columns and beams are architectural (`CL`) or structural (`SC`, `BM`). Profiles are standard or custom sections from the library.

## steps
1. Column: type `SC` ([[cmd:str.column]]) or `CL` ([[cmd:arch.column]]); pick a type and click a point. Columns at grid intersections: [[cmd:str.at_grids]].
2. `Space` rotates the column by 90 degrees before placing; the temporary dimensions to neighbouring grids and walls can be edited by typing a length.
3. Beam: `BM` ([[cmd:str.beam]]) - start and end points; the section type and the elevation are in the options bar; the tool chains beams (the end of one is the start of the next). `Space` swaps start and end.
4. Beam ends and elevation are dragged with grips; profile rotation and height are edited in the properties.
5. Grid snapping: `Tab` cycles snaps, the column lands exactly on the grid intersection.
6. A series of beams at a spacing is made by [[cmd:str.beam_system]] ([[topic:structure]]).

## keys
- `CL` / `SC` - column, `BM` - beam
- `Space` - rotate by 90 degrees (column), swap ends (beam)
- `Tab` - snap
- `Enter` / `Esc` - finish / exit

## tips
- In the options bar a column has a base offset and a top (the level above or a height).
- Profiles can be loaded from the library: [[topic:families]].
- Beams and columns are exported to IFC with their sections.
