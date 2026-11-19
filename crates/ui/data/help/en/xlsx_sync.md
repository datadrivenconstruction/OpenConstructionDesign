# Exchange with .xlsx spreadsheets
@tags xlsx, spreadsheet, import, export, preview, round-trip
@context data.import_xlsx data.export_xlsx
@commands data.import_xlsx data.export_xlsx view.schedule
@see schedules query_bulk undo_history

The model can be exported to `.xlsx`, edited in any spreadsheet program and brought back. A preview of the changes is shown before applying, and the whole import is rolled back with a single `Ctrl+Z`.

## steps
1. Export: `Ctrl+E` ([[cmd:data.export_xlsx]]), the "Data" tab or the menu "File > Export to XLSX..." - pick the file (it is offered next to the project, named after it); the model is written by categories.
2. Edit values in the file. New rows create elements, removed rows can delete, changed values edit.
3. Units can be given in a column header: `m`, `mm`, `cm`, `ft`, `in`, `ft-in`.
4. Import: "File > Import from XLSX..." or drop an `.xlsx` into the "Import" window. Addresses like `Sheet!A1` are recognised.
5. Review the preview: chips with the number of added, changed and deleted rows, new columns, errors (an "Errors only" filter). Nothing changes until you press "Apply".
6. Press "Apply". The result is one operation: `Ctrl+Z` reverts all of it. "File > Sync with XLSX" rereads the linked workbook and shows the differences.

## keys
- `Ctrl+E` - export to XLSX
- `Ctrl+Z` - revert the whole import
- Menu "File" (the project name, `Alt+F`) - export, import, sync

## tips
- Do not change the identifier column: rows are matched to elements by it.
- On very large tables (tens of thousands of rows) import may take seconds: quantities are recomputed.
