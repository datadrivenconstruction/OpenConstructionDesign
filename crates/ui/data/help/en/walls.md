# Walls
@tags wall, walls, thickness, wall type
@context arch.wall str.wall sel:wall
@commands arch.wall str.wall arch.wall_room arch.wall_pick mod.wall_edit ctx.wall.profile
@see wall_layers wall_joins wall_profile doors_windows

A wall is a layered element with an axis, a height and a type. It is drawn as a chain of segments (straight and arc); corner joints, intersections and openings are built automatically.

## steps
1. Type `WA` ([[cmd:arch.wall]]); for a bearing wall use `SW` ([[cmd:str.wall]]).
2. In the options panel pick the type, level and height.
3. Click points or type a length: `3500`, `3.5m`, `12'6"`; a relative offset from the last point is `@dx;dy`. An angle is not typed: the pointer snaps to multiples of 45 degrees.
4. `A` toggles a straight segment and an arc. `Tab` cycles the snaps under the cursor.
5. `C` closes the outline, `Enter` ends the chain, `Esc` leaves the command.
6. Select a wall - end and arc grips and temporary dimensions appear at once; layers and joints are edited in [[topic:wall_layers]] and [[topic:wall_joins]].

## keys
- `WA` - wall, `SW` - bearing wall, `WR` - wall by room outline
- `A` - arc / straight, `Tab` - next snap
- `C` - close, `Enter` - finish, `Esc` - exit
- `WL` - wall by line ([[cmd:arch.wall_pick]]): click a drawing line; `F` axis / face on the line, `Tab` the connected chain
- `Backspace` - erase the typed digit

## tips
- The wall type, level and height are set in the options bar (`H` is the height of new walls).
- Attaching the top and base to a level, roof or slab ([[topic:wall_profile]]) is planned.
- Place doors and windows after walls: openings are cut automatically ([[topic:doors_windows]]).
