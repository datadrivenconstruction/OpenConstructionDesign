# Floors and slabs
@tags floor, slab, layers, deck
@context arch.floor str.slab sel:floor
@commands arch.floor str.slab ctx.floor.shape mass.slab_by_face
@see shape_edit openings_shafts roofs

A floor is a layered element with an outline, a thickness and a type. An architectural floor is created with `FL`, a structural slab with `SB`.

## steps
1. Type `FL` ([[cmd:arch.floor]]) or `SB` ([[cmd:str.slab]]).
2. Click inside a closed wall loop - the outline is picked automatically. `Space` toggles: by axes / by inner faces (the preview shows the area).
3. Or draw the outline by points, typing lengths.
4. Pick the type and level in the options panel; the height offset is set in the properties.
5. Finish with `Enter`; the floor appears in all views and in quantities.
6. Floors that already exist (from the sample, IFC or a form) are edited with [[topic:shape_edit]].

## keys
- `FL` - floor, `SB` - structural slab
- `Space` - by axes / by faces
- `Enter` - finish, `Esc` - exit
- `Backspace` - remove the last point

## tips
- Floor openings are in [[topic:openings_shafts]]; shafts cut every floor they cross.
- A floor on a mass face: [[cmd:mass.slab_by_face]] ([[topic:mass_by_face]]).
