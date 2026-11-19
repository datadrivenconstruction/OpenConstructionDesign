# Importing DWG and DXF
@tags DWG, DXF, drawing, underlay, CAD, import
@context mgr.import_cad
@commands mgr.import_cad mgr.import_pdf mod.measure
@see import_pdf units levels

A `.dwg` or `.dxf` drawing is placed on a level as an underlay: trace walls and grids over it. Blocks (INSERT) are expanded; DXF from R2000 onwards is read.

## steps
1. Run `IC` ([[cmd:mgr.import_cad]]) or `Ctrl+O` - the "Import" window; pick the file (several files can be placed at once, each on its own level or on a new one).
2. In the "Import drawing" window set the units (auto from the file, mm, cm, m, inch, foot): a mismatch makes everything hundreds of times larger or smaller.
3. Choose the origin: as in the file, lower-left corner to (0, 0), or aligned with the first drawing; add an X / Y shift in metres if needed, then "Place".
4. Check a known dimension against the underlay (the measure command `MS`, [[cmd:mod.measure]], is planned) to make sure the scale is right.
5. Trace walls (`WA`) and grids (`GR`) over the underlay; snaps work to its lines. The "Layers..." panel also has a trace mode: click a line to get a wall of the current type.
6. The "Layers..." panel turns the drawing and its layers on and off, makes them halftone, moves / rotates / copies the drawing and removes it.

## keys
- `IC` - import DWG/DXF
- `IC` - import DWG/DXF (again)
- `Tab` - cycle snaps, including underlay lines

## tips
- With units set to "auto" the scale is read from the file header; check it by measuring a known size.
- The "Drawing -> BIM..." window recognises walls and other elements from the drawing, with a preview and block rules.
- A raster or vector `.pdf` page is placed separately: [[topic:import_pdf]].
