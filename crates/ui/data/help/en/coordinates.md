# Coordinates and georeferencing
@tags coordinates, georeference, origin, true north
@context mgr.coords
@commands mgr.coords mgr.project_info
@see linked_models topography units

Coordinates tie the project to the outside world: shared origin, rotation to north, position on the site. They are needed for links and exchange.

**Status: planned.** Project coordinates and georeferencing is not available in this build yet: the ribbon button is inactive. The steps below describe the intended workflow.

## steps
1. Open `GC` ([[cmd:mgr.coords]]).
2. Set the project origin and the rotation angle relative to north.
3. Enter coordinates and elevation of the base point (in metres or project units).
4. Confirm: background models and underlays shift according to the setup.
5. For linked models use the same origin: [[topic:linked_models]].
6. Project information (name, address) - [[cmd:mgr.project_info]].

## keys
- `GC` - coordinates
- `Esc` - close the window
- `Ctrl+Z` - undo

## tips
- Set georeferencing before you start working: a later shift affects all links.
- Units and precision: [[topic:units]].
- Coordinates are taken into account on IFC export.
