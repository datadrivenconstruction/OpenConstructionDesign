<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="crates/app/data/brand/design-lockup-dark.svg">
    <img src="crates/app/data/brand/design-lockup-light.svg" alt="OpenConstructionDesign" height="56">
  </picture>
</p>

# OpenConstructionDesign

**Your AI builds the CAD-BIM.** A free, open-source CAD-BIM editor that a cloud AI model with your own key
or a model on your own machine can drive. You describe a building; the AI drafts real walls,
rooms, doors, stairs and services in the model; you review the draft and decide what lands.

[![CI](https://github.com/datadrivenconstruction/OpenConstructionDesign/actions/workflows/ci.yml/badge.svg)](https://github.com/datadrivenconstruction/OpenConstructionDesign/actions/workflows/ci.yml)
[![Licence: AGPL-3.0](https://img.shields.io/badge/licence-AGPL--3.0-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/datadrivenconstruction/OpenConstructionDesign?include_prereleases&label=release)](https://github.com/datadrivenconstruction/OpenConstructionDesign/releases)
[![Rust](https://img.shields.io/badge/rust-1.89%2B-informational.svg)](docs/BUILDING.md)

<p align="center">
  <img src="docs/images/housing.webp" alt="Six-storey U-shaped housing block with 36 flats around a garden court, generated from a short brief and rendered by the editor" width="100%">
</p>

<p align="center"><sub>6-storey housing, 36 flats: 516 rooms, 666 walls, 7,734 elements, built by one AI agent in 28 minutes, 0 audit errors.</sub></p>

OpenConstructionDesign combines CAD precision with BIM data in one editor. It has its own
geometry kernel and file decoders, reads and writes IFC, and keeps the whole model and its
change history in a single SQLite file on your machine.

> **Status: 0.10 beta.** The editor is usable, but the project file format, the APIs and
> the interface can still change between releases. Keep the original of every model you open
> and do not use the editor as the only copy of production data. See [Current limits](#current-limits).

## Three ways to drive it

The same tools sit behind all three. The AI plans on a scratch copy of the model, runs the
model checks on its draft and shows a plan card, a 3D preview and a diff. Nothing reaches your
model until you press **Apply**, and an applied plan is one undo step.

**1. Prompts in the editor.** Open the assistant panel and ask in plain words. Bring your own
provider: any compatible HTTP chat interface, or a local model server on your own machine. Keys
stay in the OS key store; a local-only mode keeps every request on your network.

<p align="center">
  <img src="docs/images/ui-chat.webp" alt="The AI assistant panel with a drafted bike store: four walls and a door listed with quantities, highlighted on the plan, waiting for Apply" width="100%">
</p>

**2. MCP.** The editor runs an MCP server, so any MCP
client (a coding agent or a desktop assistant) uses the same tools (`get_context`, `query_elements`, `create_elements`, `run_checks`,
`request_apply`) under the same Apply rule:

```json
{ "mcpServers": { "ddc-editor": {
    "type": "http", "url": "http://127.0.0.1:47621/mcp",
    "headers": { "Authorization": "Bearer <token>" } } } }
```

**3. CLI.** The headless `ddc` tool runs without a window, for agents in a terminal and for CI:

```sh
ddc gen --brief brief.json --out housing.ddc    # a whole building from a JSON brief (ERP link build)
ddc run housing.ddc site.lua --out site.ddc     # sandboxed Lua geometry scripts
ddc open housing.ddc --export ifc4 housing.ifc  # IFC, XLSX and more
```

## Three editions

| Edition | Status | Get it |
|---|---|---|
| **Desktop editor** | Available for Windows 10/11; Linux gets the headless `ddc` CLI | [Releases](https://github.com/datadrivenconstruction/OpenConstructionDesign/releases) |
| **Browser version** | Preview (WebAssembly, WebGL2): open a project from a link, look at it, ask the assistant, save a draft. Not part of this repository yet | Coming soon |
| **ERP edition** | In preparation: the editor inside OpenConstructionERP, so estimate, schedule and orders follow the model | [openconstructionerp.com](https://openconstructionerp.com) · [GitHub](https://github.com/datadrivenconstruction/OpenConstructionERP) |

OpenConstructionDesign is part of the OpenConstruction family, with
[OpenConstructionERP](https://openconstructionerp.com) (cost estimation, scheduling,
procurement) and [OpenConstructionAcademy](https://openconstructionacademy.com) (courses and
guides on data-driven construction).

## Generation data

Measured, not estimated. Time and tokens come from each agent's own log and include reading
files and every failed attempt; elements as counted by `ddc info`.

**One agent per building, three agents at the same time on one laptop** (editor 0.10 beta):

| Project | Tokens | Agent time | Elements | Audit errors |
|---|---:|---:|---:|---:|
| 6-storey housing, 36 flats | 130,580 | 28 min | 7,734 | 0 |
| Football stadium, 30,000 seats | 127,142 | 25 min | 6,288 | 0 |
| Diagrid tower, 40 storeys, 170 m | 115,624 | 33 min | 6,566 | 0 |

**From a plain description to an IFC file** (one agent, the notes from one earlier practice
run; the clock stops when the IFC file is written):

| Project | Tokens | Time | Elements | Last generator run |
|---|---:|---:|---:|---:|
| 3-storey school, ventilation, water, heating | 57,927 | 5 min 27 s | 1,558 | 3.9 s |
| 4-storey clinic, water and drainage | 65,826 | 7 min 52 s | 2,585 | 1.8 s |
| Data centre, cooling and 2N power | 70,994 | 11 min 44 s | 1,710 | 6.3 s |

## Quantities that flow into cost estimation

The model is data first: every element carries its parameters and quantities, and they leave
the editor in forms an estimating or ERP system can read, including
[OpenConstructionERP](https://openconstructionerp.com).

<p align="center">
  <img src="docs/images/erp-link.svg" alt="Diagram: the editor publishes a revision package with element rows and quantities; the ERP turns it into a cost estimate, a schedule and procurement; a project brief from the ERP generates a model in the editor" width="100%">
</p>

Schedules and quantity take-off come straight from the model. The XLSX round trip exports
parameters, lets you edit them in any spreadsheet, and imports them back as one undo step with
a per-cell report.

- **ERP link** (build feature `link`): `ddc gen --brief` turns a project brief from the ERP
  into a model; `ddc publish` sends a model back as a revision package with element rows
  (`elements.parquet`), quantities, a diff against the parent revision, sheets, IFC and 3D
  geometry. The ERP prices the quantities; the editor never invents a cost.
- Linked sessions open from a `.ddclink` file; the project token lives only in the OS key
  store.

<!-- placeholder for the owner: docs/images/erp-cost-table.png - the ERP cost view filled from a published revision (shot on the ERP side). -->

## Generated from a brief

Free-form massing, laid out into rooms, doors, windows and stairs by the generators. The
models below were produced from short briefs and rendered by the editor itself; no
third-party models.

<p align="center">
  <img src="docs/images/geometry-collage.webp" alt="Six generated buildings on landscaped plots: Round Tower, Twisted Tower, Shell Pavilion, Stepped Block, Ring Court and Wedge Office" width="100%">
</p>

Round Tower (11 storeys, 11,159 m&sup2; GFA, 275 rooms, 2.4 s), Twisted Tower (11 storeys,
9,900 m&sup2;, 242 rooms, 1.0 s), Shell Pavilion (4 storeys, 1,374 m&sup2;, 58 rooms, 26.1 s),
Stepped Block (8 storeys, 10,692 m&sup2;, 194 rooms, 0.6 s), Ring Court (5 storeys,
15,680 m&sup2;, 372 rooms, 0.8 s), Wedge Office (6 storeys, 6,048 m&sup2;, 138 rooms, 0.5 s).
Times are model generation only. Trees and plants are stylised site dressing, not planting
design.

### Building services

Each grid steps from the architecture through a ghosted shell to the service systems, drawn
as 3D networks inside the building. Systems differ per project; not every project has every
discipline yet.

<table>
<tr>
<td width="50%">

<img src="docs/images/school-layers.webp" alt="School in four steps: architecture, ventilation, water, drainage and heating, all services together" width="100%">

School, 3 storeys, 2,592 m&sup2; GFA, 65 rooms: ventilation, then water, drainage and heating.

</td>
<td width="50%">

<img src="docs/images/clinic-layers.webp" alt="Clinic in four steps: architecture, cold and hot water, drainage and fixtures, all services together" width="100%">

Clinic, 4 storeys, 6,400 m&sup2; GFA, 194 rooms: cold and hot water, drainage and fixtures.
Water and drainage only; no ventilation yet.

</td>
</tr>
<tr>
<td width="50%">

<img src="docs/images/housing-layers.webp" alt="Housing block in four steps: architecture, supply air, supply and extract air, air with fixtures and appliances" width="100%">

Housing block, 6 storeys, 516 rooms: supply and extract air, fixtures and appliances.

</td>
<td width="50%">

<img src="docs/images/data-centre-layers.webp" alt="Data centre in four steps: architecture, chilled water and racks, power, all services together" width="100%">

Data centre, 1 storey, 2,231 m&sup2; GFA, 18 rooms: chilled water with racks, then power.

</td>
</tr>
</table>

<p align="center">
  <img src="docs/images/process-plant.webp" alt="Process plant draft: storage tanks in a bund, a jacketed reactor, a distillation column, pumps and heat exchangers on a steel pipe rack" width="100%">
</p>

Process plant draft, 90 m&sup3;/h: 29 units, 285 pipe runs, 978 m of pipe, 245 supports, from
one brief. 46 of 69 nozzles are connected so far; the plant check lists every open line.

### Plans

Ground-floor plans of the same generated models, with room names, areas, furniture and
hatched walls.

<table>
<tr>
<td width="33%"><img src="docs/images/ring-court-plan.webp" alt="Ground floor plan of Ring Court: a square ring of open-plan rooms around an open courtyard" width="100%"><br>Ring Court</td>
<td width="33%"><img src="docs/images/stepped-block-plan.webp" alt="Ground floor plan of Stepped Block: rooms around a central stair and lift core, rotated on the plot" width="100%"><br>Stepped Block</td>
<td width="33%"><img src="docs/images/wedge-office-plan.webp" alt="Ground floor plan of Wedge Office: a triangular floor with offices and open space around a core" width="100%"><br>Wedge Office</td>
</tr>
</table>

## The editor

<p align="center">
  <img src="docs/images/ui-plan3d.webp" alt="A five-storey office around a courtyard generated from one brief: ground floor plan on the left, realistic 3D view on the right, model totals in the side panel" width="100%">
</p>

<table>
<tr>
<td width="50%"><img src="docs/images/ui-select.webp" alt="A selected wall with its type, length, height, layers and phases in the properties panel" width="100%"><br>Select anything: type, length, height, layers and phases, editable in place.</td>
<td width="50%"><img src="docs/images/ui-rooms.webp" alt="Ground floor with numbered rooms, names and areas, and model totals in the side panel" width="100%"><br>Rooms are named and measured on the plan and follow every edit.</td>
</tr>
</table>

## Features

**Modeling**
- Levels, grids, reference planes, walls (layered types, automatic joins), floors, ceilings,
  roofs, stairs, railings, ramps, curtain walls, openings, doors, windows, rooms and zones.
- Own geometry kernel: exact predicates, B-rep solids, CSG, sweeps, NURBS, sketch constraints.
- Parametric families: components with types and parameters defined inside the model, with no
  separate family editor.
- Every edit is a journaled step with undo, redo, a per-change history with its author, and
  version comparison.

**Documentation and drawings**
- Plans, sections, elevations, 3D views, schedules, dimensions, tags, text, legends.
- Sheets with title blocks and views placed at scale; vector PDF and SVG export.

**Structure and MEP**
- Beams, columns, braces, structural walls and floors, footings, foundation slabs, and
  reinforcement that follows its host; steel connections with plates and bolts.
- Ducts, pipes, cable trays and conduits with fittings derived from the network; system
  checks and route proposals.

**IFC and other formats**
- Import IFC2X3, IFC4, IFC4X1 to IFC4X3 and `.ifczip`; export IFC2X3, IFC4 and IFC4.3.
- Read `.dwg` and `.dxf` with independent decoders. The original file is never modified.
- Export glTF, STEP, STL, OBJ, 3MF; exchange issues as BCF 2.1/3.0.
- `.rvt` through an optional, separately distributed bridge module that speaks the published
  [bridge protocol](docs/plugins/BRIDGE_PROTOCOL.md); it is not part of this repository.
- Clash detection, rule checks and model integrity checks.

**Scripting and automation**
- JSON edit scripts with preview, and recording of actions into a script.
- Sandboxed Lua geometry scripts (no files, network, clock or randomness).
- The headless `ddc` command-line tool (`open`, `info`, `batch`, `run`; `gen` and `publish`
  with the ERP link).

**Spreadsheets**
- Round-trip element parameters through XLSX; an import is one undo step with a per-cell
  report.
- Query elements and open them as an editable table.

## Install

- **Windows desktop.** Download the installer (Inno Setup) from the Releases page of this
  repository and run it. Windows 10 or 11, x86_64.

## Build from source (Windows)

Windows 10 or 11 on x86_64 is the primary platform. Details and other platforms:
[docs/BUILDING.md](docs/BUILDING.md).

1. Install Git, the MSVC build tools ("Desktop development with C++"; a C compiler is needed
   for the bundled SQLite and Lua) and Rust via [rustup](https://rustup.rs/) (stable, MSVC
   target, version 1.89 or newer; CI builds with 1.95.0). Then run `rustup component add rustfmt clippy`.
2. Clone and build in a new terminal:

   ```powershell
   git clone <repository-url> ocd
   cd ocd
   cargo build --release -p ddc-app -p ddc-cli
   ```

3. Run; binaries are in `target\release\`:

   ```powershell
   cargo run --release -p ddc-app             # the editor
   cargo run --release -p ddc-cli -- --help   # headless CLI (binary name: ddc)
   ```

   If the window does not open or rendering is broken, start with `--safe-graphics`.

CLI examples:

```sh
ddc info model.ifc
ddc open model.ifc --export xlsx model.xlsx --export ifc4 model-ifc4.ifc
ddc run project.ddc tower.lua --out tower.ddc --param height="40 m"
```

## Platform support

| Platform | Status |
|---|---|
| Windows 10/11, x86_64 | Supported; primary development platform; installer provided. |
| Linux | Library crates and the headless `ddc` CLI (built and tested in CI). The editor window is not supported yet. |
| macOS | Code paths exist; not regularly built or tested. |
| Browser (wasm32) | Preview; the browser build is not part of this repository yet. |

Details: [docs/BUILDING.md](docs/BUILDING.md).

## Current limits

- The `.dwg` and `.dxf` decoders are independent implementations. They cover common
  building elements and geometry, not every feature of those formats. Compare imported models
  with the source before relying on them.
- The AI assistant can make mistakes. Always read the preview and diff. Quality depends on the
  provider and model you choose; small local models handle only simple requests.
- Linux and macOS: only the library crates and the headless CLI are supported. The editor
  window is Windows-first.
- The structural solver is a library and is not yet exposed in the editor.
- The project file format is not frozen; keep an IFC export for long-term storage.
- Very large models can be slow.
- Interface languages: English and Russian; other languages are catalog stubs.

## Roadmap

Planned, with no dates: a frozen project file format, broader `.dwg` coverage, the structural
solver inside the editor, more interface languages, and editor builds beyond Windows.

Plans can change; the [changelog](CHANGELOG.md) lists what actually shipped.

## Documentation

| Document | Contents |
|---|---|
| [docs/user/getting-started.md](docs/user/getting-started.md) | First model, files, saving |
| [docs/user/ai-assistant.md](docs/user/ai-assistant.md) | Providers, preview, Apply |
| [docs/user/formats.md](docs/user/formats.md) | Supported formats and IFC round trip |
| [docs/user/families.md](docs/user/families.md) | Parametric families |
| [docs/USER_GUIDE.md](docs/USER_GUIDE.md) | Core workflows and shortcuts |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crates, data flow, journal, AI contract |
| [docs/BUILDING.md](docs/BUILDING.md) | Building and testing |

## Contributing

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md). Every contributor signs
the [Contributor License Agreement](CLA.md) before the first merge and signs off each commit
(`git commit -s`). Please follow the
[Code of Conduct](CODE_OF_CONDUCT.md).

## Security

Do not report vulnerabilities in public issues; see [SECURITY.md](SECURITY.md).

## Licence

Licensed under the [GNU Affero General Public License, version 3 only](LICENSE)
(AGPL-3.0-only). If you run a modified version as a network service, you must offer its
source to that service's users. Third-party components keep their own licences; see
[THIRD_PARTY_LICENSES.txt](THIRD_PARTY_LICENSES.txt) and [NOTICE](NOTICE). A separate commercial licence
is available for use that the AGPL does not fit; see [COMMERCIAL.md](COMMERCIAL.md).
