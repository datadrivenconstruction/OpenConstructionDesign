# Importing IFC
@tags IFC, IFCZIP, import, exchange, BIM
@commands file.open mgr.links data.export_ifc
@see import_rvt export linked_models

IFC is an open exchange format. The editor reads IFC2X3, IFC4X1/X2/X3 and IFCZIP archives. Any product with geometry becomes a native element or an editable mesh.

## steps
1. Open the file with `Ctrl+O` ([[cmd:file.open]]) or "File > Import..." and pick `.ifc` or `.ifczip`.
2. The whole file is imported at once as a single operation (one `Ctrl+Z` removes it); the report in the "Import" window lists what was created.
3. The report counts levels, walls, openings, floors, rooms, grids, curtain walls and components; the kinds that were not transferred are listed as "not transferred: kind xN".
4. If the file has no levels, a default level is created. Importing again does not update elements in this build.
5. Review the report and the warnings window ([[topic:warnings]]).
6. Attaching an IFC as a linked model is planned: [[topic:linked_models]].

## keys
- `Ctrl+O` - open / import

## tips
- IFC export is planned: [[topic:export]].
