# Wall layers and wall editing
@tags layers, wall construction, thickness, material, compound wall, wall edit
@context mod.wall_edit
@commands mod.wall_edit mgr.types mgr.materials mod.change_type
@see walls wall_joins wall_profile families

A wall type consists of layers (masonry, insulation, finish, ...), each with its own thickness and material. [[cmd:mod.wall_edit]] opens the edit panel for the selected wall: layers, joints, vertical structure, openings.

**Not available yet:** the materials list (`mgr.materials`). The ribbon buttons of these commands are inactive.

## steps
1. Select a wall and run [[cmd:mod.wall_edit]].
2. In the layers tab add, remove or reorder layers; set the thickness and the material.
3. Layer priority decides how joints are cleaned up: a higher-priority layer "cuts through" the others.
4. For compound walls split the wall by height into parts with different structure (vertical structure); layers can wrap at ends and in openings.
5. Apply changes to the type ([[cmd:mgr.types]], `TY`) to change every wall of that type; change type for the selection with [[cmd:mod.change_type]] (`CY`).
6. Check the result in a section or 3D: joints and openings are recomputed automatically.

## keys
- `TY` - type manager
- `CY` - change type of the selection
- `Esc` - close the panel without applying an unfinished step
- `Ctrl+Z` - undo the edit

## tips
- A type edit is a single operation: one `Ctrl+Z` brings everything back.
- If a layer "disappears" at a joint, check the layer priorities.
