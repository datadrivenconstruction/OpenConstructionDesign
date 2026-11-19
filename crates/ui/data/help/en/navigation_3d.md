# Navigating in 3D
@tags 3D, orbit, camera, rotate, pan, zoom
@commands view.3d view.zoom_fit view.zoom_sel view.section_box view.style
@see views_3d section_box big_files

In the 3D view the camera orbits around a point of interest. The camera never affects the model; selection and highlighting work as in plan.

## steps
1. Open 3D: `Ctrl+3` ([[cmd:view.3d]]).
2. Orbit: drag with the left mouse button.
3. Pan: drag with the middle button, or left button with `Shift` held.
4. Zoom: wheel - zooms toward the point under the cursor.
5. Fit the model: press `F` over the view or double-click the middle button; also [[cmd:view.zoom_fit]].
6. Zoom to selection: [[cmd:view.zoom_sel_3d]]; home view: [[cmd:view.home_3d]]; perspective or orthographic: [[cmd:view.projection]]; walk and fly: [[cmd:view.walk]] (`WASD`, mouse to look, `Esc` to exit).
7. To look inside, limit the model with a [[topic:section_box]] or isolate part of it ([[topic:visibility]]).

## keys
- LMB drag - orbit
- MMB drag or `Shift` + LMB - pan
- Wheel - zoom to cursor
- `F` - zoom to fit
- `Ctrl+3` - 3D view

## tips
- Clicking an element in 3D selects it; grips (wall ends, height, levels) can be dragged right in 3D.
- For heavy models see [[topic:big_files]].
