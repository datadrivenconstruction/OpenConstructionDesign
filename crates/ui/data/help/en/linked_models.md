# Linked models
@tags link, xref, external model, linked model
@context mgr.links
@commands mgr.links view.diff
@see import_rvt import_ifc checks_compare

A link attaches an external model (`.rvt`, `.ifc`, `.dwg`) as a read-only background: you see it, snap to it and run clash checks, but do not edit it.

**Status: planned.** Linked models is not available in this build yet: the ribbon button is inactive. The steps below describe the intended workflow.

## steps
1. Open `LK` ([[cmd:mgr.links]]) and add a file.
2. Set the position: shared coordinates, origin or a point.
3. Enable the link's visibility in the views you need.
4. Snap to its lines and faces when building your own elements.
5. When the source file changes, reload the link and compare versions: [[cmd:view.diff]].
6. Remove the link when it is no longer needed - the project model is unchanged.

## keys
- `LK` - linked models
- `Tab` - snap to link elements
- `Esc` - exit

## tips
- Links are not part of your schedules but take part in clash checks ([[topic:clash]]).
- For a shared origin use georeferencing: [[topic:coordinates]].
- A heavy link slows a view - turn it off where not needed.
