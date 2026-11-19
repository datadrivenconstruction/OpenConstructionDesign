# Clash detection
@tags clash, interference, collision, report
@context data.clash data.clash_report
@commands data.clash data.clash_report mod.isolate view.section_box
@see mep structure checks_compare

Clash detection finds intersections between two sets of elements (e.g. ducts and beams) on real geometry and builds a report.

## steps
1. Run `CH` ([[cmd:data.clash]]).
2. Define sets A and B: categories (ducts, pipes, cable trays, conduits, walls, slabs, columns, beams), a level or all levels, or the selection only. Choose the mode - "Hard" or "Clearance" - and set a tolerance in mm.
3. Press "Check". Found pairs are listed with status, elements, level, volume and location; "Show" highlights a pair in the views.
4. "Show" isolates the pair; "Reset isolation" brings the view back. A section box (`BX`) also helps to inspect the spot.
5. Give a pair a status (new, active, reviewed, resolved, approved) and a comment, or apply a fix: reroute above, reroute below, sleeve / opening (one `Ctrl+Z` undoes it). Then "Re-check".
6. Export the result: [[cmd:data.clash_report]] - XLSX or HTML.

## keys
- `CH` - clash

## tips
- Start with narrow pairs (pipes vs floors), not the whole model: faster and clearer.
- Import warnings and clashes are different things: see [[topic:warnings]].
- The report is handy to attach to issue notes; the summary line shows new, existing and resolved pairs.
