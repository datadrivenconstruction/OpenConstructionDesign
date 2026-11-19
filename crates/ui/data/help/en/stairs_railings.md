# Stairs, railings, ramps
@tags stair, railing, handrail, ramp
@context arch.stair arch.railing arch.ramp
@commands arch.stair arch.railing arch.ramp
@see levels floors openings_shafts

A stair connects two levels; railings are built along a run and landing; a ramp is an inclined surface with a given slope.

**Not available yet:** the ramp (`RA`). The ribbon buttons of these commands are inactive.

## steps
1. Type `ST` ([[cmd:arch.stair]]), click the start of the run and then its end; the preview shows "N risers of M".
2. In the options bar set the base and top levels, the width and the alignment line (centre / left / right); "Auto railing" builds the railing on both edges in the same step.
3. The number of risers is computed from the height between the levels; the tread is 0.28 m.
4. A standalone railing - `RL` ([[cmd:arch.railing]]): click the path points and press `Enter`, or use "Pick host" and click a stair.
5. A ramp (`RA`, [[cmd:arch.ramp]]) is planned.
6. Cut a hole in the floor above the stair with a shaft `SH` ([[topic:openings_shafts]]).

## keys
- `ST` - stair, `RL` - railing
- `Enter` - finish the railing path, `Esc` - exit

## tips
- Create the levels first: a stair connects two existing levels ([[topic:levels]]).
