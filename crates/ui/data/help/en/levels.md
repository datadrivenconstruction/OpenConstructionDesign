# Levels
@tags level, storey, floor level, elevation, height
@context mgr.level arch.level str.level mgr.level_rename mgr.level_delete view.level_up view.level_down view.underlay
@commands mgr.level arch.level mgr.level_rename mgr.level_delete view.level_up view.level_down view.underlay view.plan
@see grids walls navigation_2d

Levels define heights: walls, floors and floor plans are attached to them. A plan view always belongs to a level, and new elements are drawn on it.

## steps
1. Switch levels and see their elevations in the project browser or in a section / elevation ([[topic:sections_elevations]]).
2. Run [[cmd:mgr.level]] (`LV`): the "New level" dialog asks for a name and an elevation in metres (for example `3,000`); the level is created and made active.
3. A plan view always belongs to the active level; [[cmd:view.plan]] returns to the plan.
4. [[cmd:mgr.level_rename]] (`LR`) opens the "Level" dialog for the current level (name and elevation); [[cmd:mgr.level_delete]] (`LX`) deletes the current level.
5. Switch storeys with `Ctrl+PageUp` / `Ctrl+PageDown`.
6. To line walls up, turn on the underlay of the level below - `UL` ([[cmd:view.underlay]]).

## keys
- `LV` - new level
- `LR` - rename / change elevation, `LX` - delete
- `Ctrl+PageUp` / `Ctrl+PageDown` - level up / down
- `UL` - underlay of the level below

## tips
- Walls attached to a level follow its elevation; a wall top can be attached to the level above ([[topic:wall_profile]]).
- Level marks and extents are per view and can be dragged by grips on a section.
