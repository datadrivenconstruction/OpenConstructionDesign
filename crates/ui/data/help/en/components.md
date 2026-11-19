# Components: furniture and equipment
@tags component, furniture, equipment, family, fixture
@context arch.component sel:comp
@commands arch.component mod.change_type mgr.library
@see families doors_windows

A component is a placeable library element: furniture, plumbing fixtures, equipment. It can stand on a floor or on a wall (including mirrored).

## steps
1. Type `CM` ([[cmd:arch.component]]).
2. Pick the family and type in the options panel ([[topic:families]]).
3. Hover the target spot: the preview shows the final geometry.
4. `Space` rotates the component by 90 degrees; `Tab` switches snaps (wall, floor, grid).
5. Click to place; the command stays active - keep placing, `Esc` exits. A component needs a family in the library, otherwise the tool reports it.
6. A selected component has rotation and move grips; change type with `CY` ([[cmd:mod.change_type]]).

## keys
- `CM` - component
- `Space` - rotate by 90 degrees
- `Tab` - snap, `Esc` - exit
- `CY` - change type

## tips
- Components belong to the "Components" category of queries and of the schedule table.
- Cannot find what you need? Load a family into the library ([[cmd:mgr.library]]).
