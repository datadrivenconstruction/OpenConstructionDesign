# Section box
@tags section box, clip, cut away, inside
@context view.section_box
@commands view.section_box view.3d mod.isolate
@see views_3d big_files navigation_3d

A section box clips the 3D view to a rectangular volume: only what is inside is visible. It is the best way to look into a building and to lighten a heavy scene.

## steps
1. Open 3D (`Ctrl+3`).
2. Press `BX` ([[cmd:view.section_box]]): the command is a toggle; the box wraps the model with a 0.3 m margin.
3. Drag the box faces (handles) to move the clipping planes.
4. Orbit the camera to see the cut from the side you need.
5. Calling it again turns the box off and brings the full model back.
6. To show a single storey, move the top and bottom faces to its level.
7. Box around the selected elements: [[cmd:view.section_box_fit]]; cut fill on the faces: [[cmd:view.section_caps]]; 3D view of one level: [[cmd:view.level_3d]].

## keys
- `BX` - section box on / off
- `Ctrl+3` - 3D
- `F` - fit

## tips
- The box only affects display; the model is unchanged and stays whole in schedules.
- On large models limiting the volume speeds up drawing sharply ([[topic:big_files]]).
- The box can be combined with isolation ([[topic:visibility]]).
