# Navigating the plan
@tags plan, pan, zoom, navigation, fit
@context view.plan view.zoom_fit view.zoom_sel view.plan_here view:plan
@commands view.plan view.zoom_fit view.zoom_sel view.plan_here view.level_up view.level_down
@see navigation_3d levels snaps

The plan is the main drawing view. It shows the active level; panning and zooming never change the model and do not enter the history.

**Not available yet:** zoom to selection (`ZS`). The ribbon buttons of these commands are inactive.

## steps
1. Zoom: scroll the wheel - the view zooms about the cursor, not the centre.
2. Pan: hold the middle mouse button and drag.
3. Fit the whole model with [[cmd:view.zoom_fit]] (`Ctrl+0` or `ZF`).
4. A new plan for a level: [[cmd:view.plan]] (`PL`); jump back to the plan from 3D or a section with `Ctrl+1` ([[cmd:view.plan_here]]).
5. Change level with `Ctrl+PageUp` / `Ctrl+PageDown` ([[topic:levels]]).

## keys
- Wheel - zoom to cursor, middle button - pan
- `Ctrl+0` - zoom to fit
- `Ctrl+1` - plan here
- `Ctrl+PageUp` / `Ctrl+PageDown` - level up / down

## tips
- The snap grid step adapts to the zoom: zoom in to draw more precisely.
- You can zoom and pan while a command is running - it is not interrupted.
- Show the level below as an underlay ([[cmd:view.underlay]], `UL`) to line up walls.
