# MEP: routes
@tags duct, pipe, cable tray, conduit, fitting, MEP
@context mep.duct mep.pipe mep.tray mep.conduit mep.fitting ctx.mep.fitting mep.connect sel:mep
@commands mep.duct mep.pipe mep.tray mep.conduit mep.fitting mep.connect ctx.mep.fitting
@see mep_equipment mep_systems clash

Routes are ducts, pipes, cable trays and conduits. They are built in 3D from points with elevations; joints and bends are finished with fittings.

**Not available yet:** conduit (`CU`), manual fittings (`FI`) and connecting (`CN`). The ribbon buttons of these commands are inactive.

## steps
1. Duct `DT` ([[cmd:mep.duct]]), pipe `PI` ([[cmd:mep.pipe]]), tray `CT` ([[cmd:mep.tray]]).
2. Pick a size, elevation and system in the options bar; click the route points.
3. `Enter` finishes the route as one record; fittings at bends are added by the model.
4. Conduits, manual fittings and connecting a route to equipment are planned ([[cmd:mep.conduit]], [[cmd:mep.fitting]], [[cmd:mep.connect]]).
5. Sizes are changed in the properties.
6. Grouping into a system and the connectivity check: [[topic:mep_systems]] (planned).

## keys
- `DT` duct, `PI` pipe, `CT` tray
- `Enter` - finish, `Esc` - exit

## tips
- On a selected run, [[cmd:ctx.mep.fitting]] ("Insert fitting") puts a transition to another size at a distance from the run start, or a cap on an open end. The network checks the fitting fits; one undo step.
- Check intersections with structure: [[topic:clash]].
- Routes show in the plan and in 3D.
- Equipment and terminals: [[topic:mep_equipment]].
