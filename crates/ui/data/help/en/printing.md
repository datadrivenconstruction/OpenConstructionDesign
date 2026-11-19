# Printing and PDF
@tags print, PDF, SVG, sheet set, plot
@context view.print_sheets data.export_pdf
@commands view.print_sheets data.export_pdf view.sheet_set
@see sheets export

Sheets are output as vector PDF files (and optionally DXF): lines stay lines, text stays text, and line weights follow the settings. Printing to a printer and SVG output are not available.

## steps
1. Prepare sheets ([[topic:sheets]]) and, if needed, a set: [[cmd:view.sheet_set]].
2. Open the "Print sheets" window: [[cmd:view.print_sheets]] or the "Print..." button on a sheet; pick the range - current sheet, all, selected or a set.
3. `XP` ([[cmd:data.export_pdf]]) opens the same window.
4. Set the paper ("as on sheet" or a size, portrait / landscape) and the output: one PDF with a page per sheet or a PDF per sheet, optionally also a DXF per sheet; set the file name.
5. Start the output; the window reports the number of sheets written.
6. Open the result from the folder and check the first sheet.

## keys
- `XP` - export PDF/SVG

## tips
- A sheet set can be printed any number of times - a whole issue in one action.
- Paper sizes A0-A4 and ANSI A-E are supported.
- To hand over the model (not drawings) use [[topic:export]].
- The print window previews every page on the right. Clear the PDF box to output DXF only.
- Printing runs in the background with a progress bar and a stop button. A sheet that fails does not stop the set: the report names it and the reason. Before printing to a folder, the window lists the files that will be replaced and asks you to confirm.
