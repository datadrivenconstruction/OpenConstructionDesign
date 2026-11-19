# Ceilings
@tags ceiling, suspended, opening
@context arch.ceiling arch.ceiling_opening
@commands arch.ceiling arch.ceiling_opening
@see rooms_areas floors openings_shafts

A ceiling is a flat layered element at a given height above a level. It is usually built from a room outline.

## steps
1. Type `CG` ([[cmd:arch.ceiling]]).
2. Click inside a closed loop of walls (the outline is picked automatically; the loop under the cursor is highlighted) or draw an outline by points; clicking near the first point closes it.
3. In the options bar pick the ceiling type and the height above the level (2.7 m by default).
4. A hand-drawn outline is finished with `Enter` (at least 3 points).
5. A hole in a ceiling (hatch, light, shaft) - [[cmd:arch.ceiling_opening]]: draw the outline.
6. Check the ceiling in a section: it is shown at its own height.

## keys
- `CG` - ceiling
- `Enter` - finish; `Esc` - clear the entered points, a second `Esc` - exit
- `Backspace` - remove the last point
- `Ctrl+Z` - undo

## tips
- A shaft automatically cuts the ceilings on its way ([[topic:openings_shafts]]).
- Change ceiling height in the properties - select several and change the common value at once.
