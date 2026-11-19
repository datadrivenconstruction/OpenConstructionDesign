# Forms (masses)
@tags mass, form, volume, extrude, push pull, revolve, sweep
@context mass.extrude mass.revolve mass.sweep mass.blend mass.swept_blend mass.void mass.push_pull mass.fillet mass.chamfer mass.shell mass.offset mass.split_face mass.push_region mass.draft mass.paint sel:mass
@commands mass.extrude mass.revolve mass.sweep mass.blend mass.swept_blend mass.void mass.push_pull mass.fillet mass.chamfer mass.shell mass.offset mass.split_face mass.push_region mass.draft mass.paint
@see mass_booleans mass_by_face views_3d

The "Forms" tab is for conceptual volume design. A form is a solid with stable faces; later you build walls, floors, roofs and curtain walls on its faces.

## steps
1. Run [[cmd:mass.extrude]] and click the outline points in plan; `Enter` closes the outline, then type the height. Other ways: [[cmd:mass.revolve]] (centre, radius, height), [[cmd:mass.sweep]] (path, section size), [[cmd:mass.blend]], [[cmd:mass.swept_blend]].
2. A void form [[cmd:mass.void]] cuts volume from intersecting solids.
3. In 3D pick a face; [[cmd:mass.push_pull]] drags it: drag with a live preview or type a distance and press `Enter`.
4. Pick two adjacent faces and type the radius for [[cmd:mass.fillet]] (round) or the leg for [[cmd:mass.chamfer]] (chamfer); hollow out with [[cmd:mass.shell]] (open faces or `Enter` for a closed shell, type the thickness); move faces with [[cmd:mass.offset]].
5. Split a face in the middle ([[cmd:mass.split_face]]) and paint it with the next material ([[cmd:mass.paint]]).
   Draw an outline right on a flat face with [[cmd:mass.push_region]] (the face is the work plane; click the first corner or double click the last, type the height) and tilt faces with [[cmd:mass.draft]] (type the angle; a horizontal face sets the neutral plane).
   Freeform: [[cmd:mass.freeform]] spans a smooth surface over two plan corners (type the rise); pick it in 3D and type the panel size for [[cmd:mass.panelize]], the roof thickness for [[cmd:mass.surface_shell]] or the shell thickness for [[cmd:mass.thicken]].
6. Next: boolean operations ([[topic:mass_booleans]]) and elements by face ([[topic:mass_by_face]]).

## keys
- `Push/Pull` in 3D: drag a face or type a distance
- `Enter` - apply, `Esc` - cancel
- `Ctrl+Z` - undo, `Ctrl+3` - 3D

## tips
- Faces carry stable labels: elements built on a face follow it when the form changes.
- If a face disappears the element is left "orphaned" with a warning rather than vanishing.
- To estimate volume and area use schedules ([[topic:schedules]]).
