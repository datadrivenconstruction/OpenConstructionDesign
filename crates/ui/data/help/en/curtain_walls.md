# Curtain walls
@tags curtain wall, mullion, panel, glazing, facade
@context arch.curtain_wall sel:curtain mod.curtain_edit
@commands arch.curtain_wall mod.curtain_edit mass.curtain_by_face
@see walls doors_windows masses

A curtain wall is a glazed facade made of a grid of segments with mullions (vertical and horizontal profiles) and panels (glass, solid, door).

## steps
1. Type `CW` ([[cmd:arch.curtain_wall]]) and pick the start and end, as for a regular wall.
2. The tool works like the wall tool: chains of segments, `A` toggles an arc. The grid spacing (U and V) is set in the properties of the selected curtain wall.
3. Select the curtain wall and open [[cmd:mod.curtain_edit]]: edit the grid by segments, add and remove mullions.
4. Mullions can be pinned so they are not recomputed when the wall changes.
5. Change the panel type of the selected cell: glass, solid, door.
6. Corner joints, slope and curved curtain walls are also available in the edit panel.

## keys
- `CW` - curtain wall
- `Tab` - cycle snaps, `A` - arc / straight
- `Enter` - finish, `Esc` - exit
- `Ctrl+Z` - undo

## tips
- A curtain wall on a mass face: [[cmd:mass.curtain_by_face]] ([[topic:mass_by_face]]).
- Pinned mullions do not move until you release them.
- To get a door in a curtain wall replace a panel with a door panel.
