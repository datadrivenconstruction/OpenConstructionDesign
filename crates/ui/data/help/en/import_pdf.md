# PDF as an underlay
@tags PDF, underlay, plan, scale, trace
@context mgr.import_pdf
@commands mgr.import_pdf mod.measure
@see import_cad units

A `.pdf` page with a plan is placed on a level as an underlay: trace walls over it. Vector lines of the PDF take part in snapping.

## steps
1. Run `IP` ([[cmd:mgr.import_pdf]]) or pick the `.pdf` in the "Import" window (`Ctrl+O`).
2. In the "Import drawing" window pick the level, the units and the origin.
3. Set "PDF scale 1:" (100 by default): the sheet is scaled by this ratio. Calibrating by two points is not available.
4. Press "Place". The underlay can be moved and rotated in the "Layers..." panel (angle, shift).
5. Trace walls with `WA`; PDF vector lines are caught by snaps.
6. Verify lengths against a known size (the measure command `MS`, [[cmd:mod.measure]], is planned).

## keys
- `IP` - PDF underlay
- `MS` - measure

## tips
- A PDF with vector lines snaps well; a raster scan has no lines to snap to.
- The scale is a ratio only: check it by a known size and correct the ratio if needed.
