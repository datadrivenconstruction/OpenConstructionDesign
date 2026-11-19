# Sheets and viewports
@tags sheet, viewport, titleblock, paper, A1
@context view.sheet view.place view.sheet_set view.sheets_for_levels view:sheet
@commands view.sheet view.place view.sheet_set view.sheets_for_levels
@see printing sections_elevations schedules

A sheet is a drawing page of a standard size (A0-A4, ANSI A-E) on which views are placed: level plans, sections, elevations and schedules. Each placement is a viewport with its own scale.

## steps
1. Create a sheet: `SS` ([[cmd:view.sheet]]) - the "New sheet" window: number, name, paper and orientation, title block (ISO 7200 or GOST R 21.101).
2. Place a view: `PV` ([[cmd:view.place]]) or the "Place view" button - pick a level plan, a section, an elevation or a schedule and click the position on the sheet.
3. Select the viewport: set its scale ("Fit" picks one), drag it, set the view title, reset the crop.
4. Sheets for all levels at once - [[cmd:view.sheets_for_levels]]; group sheets for an issue - [[cmd:view.sheet_set]].
5. Fill in the title block fields and revisions in "Sheet properties".
6. Print or export: [[topic:printing]].

## keys
- `SS` - new sheet, `PV` - place view
- `Del` - remove a viewport from the sheet (the view stays in the project)

## tips
- A view placed on a sheet stays in the project browser: model edits show on the sheet at once.
- View scale controls line weights and text height: on the sheet they keep their real paper size.
- Schedules are placed with `PV` too.
- "Place sheet list" adds a table of the project's sheets that updates by itself. The place window also offers material, wall type and line legends.
- A long schedule splits into columns with a repeated header; the bar above the selected table sets the column height and gap.
- Double-click a title block cell to set its value on this sheet. Values shared by all sheets come from [[cmd:mgr.project_info]].
- Long text in a schedule wraps inside its column. A schedule splits into columns within the frame. If rows still do not fit, the sheet says how many are not shown, and the toolbar and the print report warn you.
- Section marks on plans show the view number and sheet number and update when sheets are renumbered.
