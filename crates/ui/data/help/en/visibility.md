# Visibility and view graphics
@tags visibility, hide, isolate, filters, view template, graphics
@context mod.hide mod.isolate mod.reveal mod.apply_hide view.hide_cat view.filters view.template view.detail_level view.thin_lines
@commands mod.hide mod.isolate mod.reveal mod.apply_hide view.hide_cat view.filters view.template view.detail_level view.thin_lines
@see big_files section_box selection

Visibility is per view: you can temporarily hide or isolate elements, hide a category, set up filters and apply a view template. The model is untouched - only what you see changes.

**Not available yet:** hiding a category (`HC`), visibility filters (`VV`), view templates (`VT`), detail level and thin lines (`TL`). The ribbon buttons of these commands are inactive.

## steps
1. Select elements and press `HH` ([[cmd:mod.hide]]) to hide them, or `HI` ([[cmd:mod.isolate]]) to keep only them.
2. Hiding a whole category ([[cmd:view.hide_cat]], `HC`) is planned.
3. Bring everything back with [[cmd:mod.reveal]] (`HR`). While hiding or isolating the view gets a frame and a badge; the cross on the badge also resets.
4. To make hiding permanent for the view, apply it: [[cmd:mod.apply_hide]] (`HA`).
5. Rule-based graphics ([[cmd:view.filters]]), view templates ([[cmd:view.template]]), the detail level and thin lines ([[cmd:view.thin_lines]]) are planned.

## keys
- `HH` - hide, `HI` - isolate, `HR` - reveal
- `HA` - apply to view

## tips
- Hiding and isolation work in plan, section, elevation and 3D; a door or window is visible only together with its host.
- Isolation is the main tool for large models: [[topic:big_files]].
- Hidden elements still appear in schedules and exports - they are not deleted.
