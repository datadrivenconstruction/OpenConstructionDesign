# Export: IFC, glTF, STL, OBJ, PDF, xlsx
@tags export, IFC, glTF, PDF, xlsx, exchange
@context data.export_ifc data.export_gltf
@commands data.export_ifc data.export_gltf data.export_pdf data.export_xlsx
@see printing xlsx_sync import_ifc

The model is exported to open formats: IFC for exchange between BIM tools, glTF for viewing and visualisation, PDF/SVG for sheets, `.xlsx` for schedules.

**Not available yet:** IFC export (`IF`) and the `XE` shortcut. The ribbon buttons of these commands are inactive.

## steps
1. IFC: `IF` ([[cmd:data.export_ifc]]) - pick the version and scope (whole project or selection).
2. glTF: `XG` ([[cmd:data.export_gltf]]) - the whole model, all levels, with element ids and properties in node extras. What is hidden or cut in the 3D view is exported too.
   STL: `XS` ([[cmd:data.export_stl]]) and OBJ: `XO` ([[cmd:data.export_obj]]) - the whole model or the selection, in metres or millimetres. OBJ writes an `.mtl` file next to it, one group per element named by its id.
3. Sheets to PDF/SVG: `XP` ([[cmd:data.export_pdf]]) - [[topic:printing]].
4. Spreadsheet: menu "File > Export to XLSX..." and the schedule table menu ".xlsx export" - [[topic:xlsx_sync]].
5. Pick a file name in the save dialog; the status line reports the result.
6. To verify, open the result in a third-party viewer or import it back ([[topic:import_ifc]]).

## keys
- `XP` - PDF/SVG

## tips
- Run the warnings check before handing over: [[topic:warnings]].
- Walls, openings, floors, rooms and curtain walls are exported as native IFC classes.
