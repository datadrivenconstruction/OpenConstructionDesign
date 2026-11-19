# Openings and shafts
@tags opening, shaft, hole, cut, void
@context arch.shaft arch.opening str.slab_opening
@commands arch.shaft arch.opening str.slab_opening arch.roof_opening arch.ceiling_opening arch.dormer
@see floors roofs ceilings doors_windows

An opening cuts a hole in one host element; a shaft is a vertical opening that cuts every floor, roof and ceiling on its way.

**Not available yet:** the wall opening (`OP`). The ribbon buttons of these commands are inactive.

## steps
1. Wall opening: `OP` ([[cmd:arch.opening]]) is planned; for now cut a door or window into the wall ([[topic:doors_windows]]).
2. Floor opening: `SO` ([[cmd:str.slab_opening]]) - draw the outline in plan.
3. Shaft: `SH` ([[cmd:arch.shaft]]) - click the outline points, `Enter` finishes.
4. Roof and ceiling openings: [[cmd:arch.roof_opening]] and [[cmd:arch.ceiling_opening]]; a dormer - [[cmd:arch.dormer]].
5. The opening outline is edited with grips.
6. The result shows in plan, section and 3D.

## keys
- `SO` - slab opening
- `SH` - shaft
- `Enter` - finish the outline, `Esc` - exit

## tips
- A shaft cuts every floor, roof and ceiling it crosses - no need to edit each one.
- To remove an opening delete its element; the host is restored whole.
