# Selecting elements
@tags select, selection, box select, filter, pick
@context edit.select_all mod.select_same_cat mod.select_same_cat_project mod.select_similar_project
@commands edit.select_all mod.select_same_cat mod.select_same_cat_project mod.select_similar_project mod.select_similar mod.select_filter
@see query_bulk modify visibility

Selection is the idle state of the editor: every action starts with selected elements. A selected element immediately shows grips and temporary dimensions; there is no separate edit mode.

## steps
1. Click an element. Clicking empty space clears the selection.
2. `Ctrl`+click adds an element to the selection; `Shift`+click excludes an element from it. Dragging a box selects: left to right takes only what is fully inside (window), right to left takes everything it touches (crossing).
3. `Tab` over the cursor cycles candidates under it.
4. Select everything in the view with `Ctrl+A` ([[cmd:edit.select_all]]).
5. Widen the selection: [[cmd:mod.select_same_cat]] (same category in view) or [[cmd:mod.select_same_cat_project]] (whole project).
6. Narrow it with a filter ([[cmd:mod.select_filter]], `SX`) - by category, pinned, links.
7. The selection breakdown is shown in the status bar.

## keys
- `Ctrl` + click - add
- `Ctrl` + box - add, `Shift` + click - exclude
- `Tab` - next candidate
- `Ctrl+A` - select all in view
- `Ctrl+Shift+A` - select similar
- `Esc` - clear selection

## tips
- Dragging a selected element moves it by its grips; copying is done with the copy command.
- For conditions like "doors wider than 900" use a query ([[topic:query_bulk]]).
