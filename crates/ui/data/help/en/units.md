# Units and length input
@tags units, metric, imperial, feet, inches, input
@context mgr.units
@commands mgr.units mgr.settings
@see dimensions walls xlsx_sync

Units define how lengths, areas and angles are displayed and entered. Metric and imperial systems are supported; you can type in either, regardless of the chosen one.

## steps
1. `UN` ([[cmd:mgr.units]]) switches the display system (metric / imperial); there is no separate dialog for length unit or precision.
2. Type lengths into the cursor input without selecting a field: `3500`, `3.5m`, `350cm`, `12'6"`.
3. Typed lengths accept a unit suffix; a relative offset from the last point is `@dx;dy`. Expressions like `3000+150` are not evaluated.
4. A number without a unit is read in project units.
5. In schedules and `.xlsx` the unit can be given in the column header ([[topic:xlsx_sync]]).
6. Display follows the chosen system everywhere: the status line, dimensions and the properties panel.

## keys
- Digits and `. , ' " @` go straight to the input line
- `Enter` - apply the number, `Esc` - clear
- `Backspace` - erase a character

## tips
- Type numbers right while drawing: `WA`, click, `3500`, `Enter`.
- Unit letters (`m`, `cm`, `ft`) are accepted only after a number.
- Changing units does not change the model, only the display.
