# Schedules and tables
@tags schedule, table, quantities, takeoff, bill
@context view.schedule data.table
@commands view.schedule data.table data.export_xlsx data.query
@see xlsx_sync query_bulk sheets

A schedule is a live data view of the model: rows are elements of the chosen category, columns are parameters. It updates on every model change and is synchronised with selection in plan and 3D.

**Not available yet:** the commands `TB`, `TD`, `XE` and placing a schedule on a sheet. The ribbon buttons of these commands are inactive.

## steps
1. Open the "Model + table" interface (`Ctrl+Alt+5`, [[topic:workspaces]]): the schedule table sits next to the model.
2. Choose the columns ("Columns" menu), filter rows ("Filter": text is a substring; numbers `>5`, `<=3000`, `=2,5`, `<>0`; `!` negates), group and get subtotals ("Groups"). Totals are computed automatically.
3. Computed columns: the "Formulas" menu - a column is an expression such as `[Length] * [Height]`, `round(x; 2)`, `if(a > b, 1, 0)`.
4. Click a row - the element is highlighted in the views; selection works in reverse too.
5. For bulk edits export the table through the table menu ".xlsx export" and come back with a preview: [[topic:xlsx_sync]].
6. Rows can also be limited by phase ("Phase" filter: all, built in a phase, demolition schedule) - [[topic:phases]].

## keys
- `Ctrl+Alt+5` - "Model + table" interface

## tips
- Quantities come from real geometry: layers, openings and cuts are accounted for.
- Wall columns include length, height, thickness, plan area and side area.
- Need conditions ("doors wider than 900")? Use a query: [[topic:query_bulk]].
