# Phases and demolition
@tags phases, demolish, renovation, existing, new construction
@context mgr.phases mod.demolish
@commands mgr.phases mod.demolish view.filters
@see visibility schedules

Phases describe the life of a project: existing, demolished, new. Each element remembers the phase it was created in and when it was demolished, and a view shows a chosen phase.

## steps
1. Open [[cmd:mgr.phases]] (`PH`) - the "Phases" window: the phase order (double-click renames), the "Current" button (the phase of new elements), add a phase.
2. In "View phase and filter" set the phase and the filter for the plan: no filter, phase result, survey (existing), demolition, project, new only, all phases; there are also plan presets "Survey", "Demolition", "Project".
3. New elements receive the current phase.
4. Demolition: select elements and press `DX` ([[cmd:mod.demolish]]) - they are marked demolished in the current phase; the window lists them with a restore option.
5. The filter decides what is shown: survey shows the existing, demolition shows what is removed, project shows the result.
6. The schedule table can be limited by phase ([[topic:schedules]]).

## keys
- `PH` - phases, `DX` - demolish

## tips
- Demolition does not delete an element: it can be restored and counted in demolition estimates.
