# Doors and windows
@tags door, window, opening, swing, hinge
@context arch.door arch.window sel:opening mod.distribute
@commands arch.door arch.window mod.distribute mod.change_type arch.opening
@see walls openings_shafts families

Doors and windows are inserted into walls; the wall opening and its layers are built automatically and follow the element when it is moved.

## steps
1. Type `DO` ([[cmd:arch.door]]) or `WN` ([[cmd:arch.window]]).
2. Pick a type in the options bar (width, height, sill) or load one from the library - [[topic:families]].
3. Hover a wall: the preview snaps along the wall axis and shows an error if the element would not fit (outside the wall, above it, overlapping another one).
4. `Space` - flip the side, `F` - flip the hinge.
5. Click to insert. Or type a number and press `Enter`: it is the distance from the wall end nearest to the cursor to the edge of the opening.
6. Place a row of identical windows with [[cmd:mod.distribute]] (`DB`): pick a door or window, type a count, `Enter`; `Space` toggles equal spacing / step.

## keys
- `DO` - door, `WN` - window
- `Space` - side, `F` - hinge
- `DB` - distribute along wall

## tips
- Change the type of the selection with `CY` ([[cmd:mod.change_type]]).
- A selected door shows temporary dimensions; click a number and type a new value.
