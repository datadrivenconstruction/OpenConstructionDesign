# Wall profile and attaching the top
@tags profile, wall top, attach to roof, gable, wall base
@context ctx.wall.profile ctx.wall.attach ctx.wall.detach
@commands ctx.wall.profile ctx.wall.attach ctx.wall.detach mod.wall_edit
@see walls roofs floors levels

A wall does not have to be rectangular: its top can be attached to a roof, floor or level, and its elevation outline edited by hand (gables, cut-outs, grooves).

## steps
1. Select a wall (or several).
2. To attach top or base run [[cmd:ctx.wall.attach]] ("Attach top/base") and pick a roof, floor or level.
3. The top now follows the chosen surface: when the roof changes, the wall is rebuilt.
4. Detach with [[cmd:ctx.wall.detach]] ("Detach top").
5. A free outline: select one wall and run [[cmd:ctx.wall.profile]] ("Edit profile"). Start from a rectangle, gable or slope, drag the points on the preview or type them, add or remove points, then "Apply" - one undo step. "Remove profile" brings the rectangle back.

## keys
- `Ctrl+Z` - undo

## tips
- A wall base can be attached to a floor slab, the top to the slab above or to a roof.
- Gable walls come from attaching the top to a pitched roof ([[topic:roofs]]).
