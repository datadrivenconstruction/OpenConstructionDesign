# Systems, auto-route, checks
@tags system, auto-route, connectivity, check
@context mep.system mep.route mep.check_system
@commands mep.system mep.route mep.check_system mep.connect
@see mep mep_equipment clash

A system groups equipment and routes of one purpose (ventilation, heating, water). Auto-route lays a route between two points, and the check finds breaks.

**Status: planned.** MEP systems, auto-routing and the connectivity check is not available in this build yet: the ribbon button is inactive. The steps below describe the intended workflow.

## steps
1. Select equipment and routes and press `SY` ([[cmd:mep.system]]) - a system is created; set its name and type.
2. Lay a route automatically: `RU` ([[cmd:mep.route]]) - pick start and end points or connectors.
3. Check the result in plan and 3D; adjust with grips if needed.
4. `CS` ([[cmd:mep.check_system]]) - connectivity check: finds unconnected ends and isolated segments.
5. Fix the issues (`CN`, [[cmd:mep.connect]]) and recheck.
6. Run clash detection against structure: [[topic:clash]].

## keys
- `SY` - system, `RU` - auto-route, `CS` - check
- `CN` - connect
- `Esc` - exit

## tips
- Auto-route avoids obstacles, but review the final geometry.
- Systems can be used in schedules and visibility filters.
- Broken systems appear in the warnings list ([[topic:warnings]]).
