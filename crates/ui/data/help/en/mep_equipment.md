# Equipment and fixtures
@tags equipment, fixture, light, outlet, air terminal
@context mep.equipment mep.terminal mep.fixture mep.device mep.light
@commands mep.equipment mep.terminal mep.fixture mep.device mep.light mep.connect
@see mep mep_systems components

Devices are placed like components but have connectors that routes attach to.

**Not available yet:** air terminals (`AT`), plumbing fixtures (`PF`), electrical devices (`ED`), lights (`LF`) and connecting a route (`CN`). The ribbon buttons of these commands are inactive.

## steps
1. Equipment - `ME` ([[cmd:mep.equipment]]): it is placed as a component ([[topic:components]]).
2. Air terminals, fixtures, electrical devices and lights ([[cmd:mep.terminal]], [[cmd:mep.fixture]], [[cmd:mep.device]], [[cmd:mep.light]]) are planned.
3. Pick a type in the options bar, click the position; `Space` rotates by 90 degrees.
4. Connecting a route to a device connector (`CN`, [[cmd:mep.connect]]) is planned.
5. Group everything into a system: [[topic:mep_systems]].

## keys
- `ME` - equipment
- `Space` - rotate by 90 degrees

## tips
- New families are loaded through the library ([[topic:families]]).
