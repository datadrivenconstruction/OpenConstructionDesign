# Formats and IFC round trip

| Format | Open | Save or export |
|---|---|---|
| `.ddc` project | yes | yes |
| IFC2X3, IFC4, IFC4X1 to IFC4X3, `.ifczip` | yes | IFC2X3, IFC4, IFC4.3 |
| `.rvt` | through an optional bridge module, distributed separately (see below) | no |
| `.dwg`, `.dxf` | yes (own decoder, partial coverage) | no |
| XLSX | yes | yes (round trip of parameters) |
| BCF 2.1 and 3.0 | yes | yes |
| glTF, STEP, STL, OBJ | yes | yes |
| 3MF | no | yes |
| PDF | underlay | sheets |
| SVG | no | sheets |

## `.rvt` files

`.rvt` is opened through an optional, separately distributed bridge module (not part of this repository) that speaks the published bridge protocol ([docs/plugins/BRIDGE_PROTOCOL.md](../plugins/BRIDGE_PROTOCOL.md)). This repository contains only the
protocol and the host that launches a signed bridge; without a bridge, `.rvt` files do not open.

## IFC round trip

Import an IFC file, edit it, export again. Identifiers and property sets are kept for elements
you did not touch. Re-import the exported file and compare it with the model before sending it
on.

## Known gaps

- Decoders for closed formats do not cover every feature. Compare the result with the source.
- Elements with no equivalent in the target schema are exported as generic proxies.
- Export to IFC for archiving; the `.ddc` file format is not frozen before 1.0.
