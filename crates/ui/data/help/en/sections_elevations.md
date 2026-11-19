# Sections and elevations
@tags section, elevation, cut, facade
@context arch.section view.section view.elevation view.section_here view:section
@commands arch.section view.section view.elevation view.section_here
@see views_3d sheets levels

A section shows an exact cut of the model along a line in plan, with visible lines beyond the plane. Elevations are built per compass direction.

**Not available yet:** the "section here" shortcut `Ctrl+2`. The ribbon buttons of these commands are inactive.

## steps
1. Section: `SN` ([[cmd:view.section]]) or [[cmd:arch.section]] - three clicks in plan: the start of the line, its end, then the depth and the viewing side.
2. The view opens in a tab and in the project browser.
3. All four elevations at once: `EV` ([[cmd:view.elevation]]).
4. Hiding and isolation work in sections as in plan ([[topic:visibility]]).
5. Place sections on sheets ([[topic:sheets]]).

## keys
- `SN` - section, `EV` - elevations
- `Ctrl+1` - back to plan

## tips
- A section is built from exact geometry: wall layers, openings and floors look as built.
- Dimensions and elevations in a section are the same `DI`, `SE` ([[topic:dimensions]]).
