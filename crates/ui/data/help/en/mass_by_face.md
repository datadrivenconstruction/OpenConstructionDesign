# Elements by form face
@tags by face, mass floors, wall by face, slab by face
@context mass.wall_by_face mass.curtain_by_face mass.slab_by_face mass.roof_by_face mass.floors
@commands mass.wall_by_face mass.curtain_by_face mass.slab_by_face mass.roof_by_face mass.floors
@see masses walls floors roofs curtain_walls

From a form you get real building elements: walls and curtain walls on vertical faces, floors on horizontal ones, roofs on upper ones. Binding is by face label, so elements follow the form.

## steps
1. Run a command and click a face of a form in 3D.
2. [[cmd:mass.floors]] - "Mass floors": sections of the form at levels, floor areas are computed at once.
3. [[cmd:mass.wall_by_face]] - click a vertical face; [[cmd:mass.curtain_by_face]] makes a curtain wall.
4. [[cmd:mass.slab_by_face]] - click a horizontal face (a floor); [[cmd:mass.roof_by_face]] - click a top or sloped face (a roof).
5. Change the form (`Push/Pull`, offset): walls and floors are rebuilt.
6. If a face is gone the element stays with a warning - rebind it.

## keys
- `Enter` - apply
- `Esc` - exit
- `Ctrl+Z` - undo, including the binding cascade

## tips
- When a form is deleted, bound elements are handled by a cascade; undo restores everything.
- Wall and floor layers come from the chosen type: [[topic:wall_layers]].
- Good for quick options: change the mass without redrawing the building.
