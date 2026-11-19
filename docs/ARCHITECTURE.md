# Architecture

OpenConstructionDesign is a CAD-BIM editor: a Cargo workspace of focused crates. The model lives in SQLite,
geometry is computed by an in-house kernel, and every change — from a mouse gesture, a
spreadsheet, a script, the AI assistant or an external agent — reaches the model as a journaled
operation.

## Crates

Each crate's `lib.rs` opens with a module map; read it before the code.

| Crate | Package | Role |
|---|---|---|
| `crates/model` | `ddc-model` | The model in SQLite: schema and migrations, elements and types (walls, slabs, roofs, stairs, curtain walls, structure, rebar, MEP, rooms, annotations, views, sheets, phases, options), the operation journal and undo, integrity and rule checks, merge, version comparison, the AI dry-run copies. |
| `crates/geom` | `ddc-geom` | Geometry kernel: exact predicates, polygons, profiles, B-rep solids, CSG, sweeps, NURBS, meshes and repair, sketch constraint solver, sections, stairs, roofs, framing, rebar shapes, form finding. No dependency on the model. |
| `crates/solver` | `ddc-solver` | Geometric constraints on plan points (distance, fix, parallel, perpendicular, collinear, equal spacing). Damped Gauss–Newton with a minimum-norm step, so edits move as few points as possible; unsatisfied constraints are reported as a conflict. |
| `crates/ops` | `ddc-ops` | The single serialisable edit language: versioned `Envelope` of groups of `Op`s, stable references, explicit units. Wire types only; JSON schema in `docs/schema/ops.v1.json`. |
| `crates/io` | `ddc-io` | Import and export: IFC (STEP reader/writer), `.dwg` and `.dxf` readers, PDF and SVG writers, XLSX round-trip, BCF, glTF, STEP, STL, OBJ, 3MF, terrain sources, revision packages, and the host for external bridge processes. |
| `crates/bridge-proto` | `ddc-bridge-proto` | Dependency-free codec of the bridge protocol: framed, versioned, neutral building data on a bridge process's stdout. |
| `crates/render` | `ddc-render` | wgpu viewport renderer: scene chunks with culling and instancing, camera and navigation, GPU picking, visual styles, section caps, adaptive quality, offscreen rendering. No UI dependency. |
| `crates/ui` | `ddc-ui` | Application shell kit on egui: design tokens and themes, icons, command registry, fuzzy command palette, ribbon, status bars, notifications, keymap, localisation. |
| `crates/app` | `ddc-app` | The editor: tools and interaction sessions, plan/section/3D/sheet views, panels, file handling, the AI assistant integration, scripts window. Binary `ddc-app`; feature `gui` (default) brings the window and GPU. |
| `crates/ai` | `ddc-ai` | Assistant core without UI or live-model access: provider profiles and wire protocols, conversation loop with limits and cancellation, tool catalog, egress policy, key storage, MCP server. |
| `crates/script` | `ddc-script` | Sandboxed Lua geometry scripts that produce one ops envelope. |
| `crates/gen` | `ddc-gen` | Pure, seed-deterministic generators and component templates. Results are schematic starting points, not checked designs. |
| `crates/fem` | `ddc-fem` | Linear static 3D frame finite elements; pure, no model dependency. |
| `crates/calc` | `ddc-calc` | Structural analysis job: analytical model and loads to solver input, run, equilibrium check, stored results. |
| `crates/cli` | `ddc-cli` | The headless `ddc` binary: `open`/`info`/`batch`/`run`. |
| `crates/link` | `ddc-link` | Optional client for a project server (feature `link`): link files, tokens in the OS key store, locks and revisions. |

Dependency direction (simplified): `fem`, `solver`, `ops`, `bridge-proto`, `ui` have no
internal dependencies; `geom` → `fem`, `solver`; `model` → `geom`, `ops`, `solver`;
`io` → `model`, `geom`, `ops`, `bridge-proto`; `render` → `geom`; `ai` → `model`, `ops`,
`gen`, `script`; `app` sits on top of all of them; `cli` uses `model`, `io`, `script` and the
windowless part of `app`.

## Data flow

```
 input ──► tool session / XLSX / script / AI plan / MCP agent
                │  (typed edit: Op envelope or model call)
                ▼
          ddc-model  ── one journal entry per edit (inverse SQL + actor + forward ops)
                │  change hook: changed rows since a counter
                ▼
   derived data: joins, cuts, rooms, quantities  ──► ddc-geom (bodies, sections)
                │
                ▼
     views: plan / section / sheet (2D), 3D scene chunks ──► ddc-render (wgpu) ──► egui shell
```

- **Model.** A project is one `.ddc` file: an SQLite database with tables for elements,
  types, parameters, views, sheets and the journal. Opening runs schema migrations and
  integrity checks.
- **Regeneration.** A SQLite update hook records every changed row. Views and caches keep a
  counter and read only what changed after it; overflow means "everything changed".
- **Geometry.** Bodies are computed from parameters by `ddc-geom` and cached; imported meshes
  are stored once and instanced.
- **Rendering.** The 3D scene is split into chunks with culling, instancing and adaptive
  quality; 2D views are drawn from cut geometry of the same model. 2D and 3D are windows into
  one document.
- **Import.** Importers write native elements, generic meshes with properties, and stable
  external references so a re-import updates instead of duplicating. An import is one undo
  step; a failure rolls the transaction back.

## Transactions and the journal

- Every edit runs in one SQLite transaction and writes one journal row (`op`). The row holds
  the **inverse SQL** of the edit, so undo works the same way for every table.
- The row also records the **actor** (user, spreadsheet, script, assistant, agent), a title,
  an optional reason, and, for edits that came as an envelope, the **forward ops**. History
  shows who did what; recorded envelopes can be replayed as scripts.
- Composite edits (`Model::atomic`, `Model::step`) nest as savepoints inside one transaction:
  one gesture or one applied plan is one undo step, all-or-nothing.
- Derived data (joins, cut-backs) can be held during a batch and settled once, merged into the
  same journal entry.
- The journal also drives three-way merge with a colleague's copy (common base found through
  shared journal history), version comparison and named checkpoints.

## AI assistant: plan → diff → Apply

The assistant never edits the live model. The contract:

1. **Read.** In read-only mode tools only query: element cards, summaries, measurements,
   checks. Text from models and files is treated as data and sanitised.
2. **Plan.** In propose mode the assistant builds a plan of intents. A worker thread compiles
   them into exact model steps on a **scratch copy** (`work.db`) next to an untouched **base
   copy** (`base.db`), taken by online backup without blocking the UI.
3. **Diff.** The plan card is built from a row-level database diff between base and scratch
   (`ATTACH` + `EXCEPT`), never from the model's own description. Validation reports only
   problems the plan introduces. The viewport draws the draft over the model; changed and
   deleted elements are shown as they will land.
4. **Apply.** Only a person's Apply, on the UI thread, writes to the live model: the plan
   becomes one journal entry (one undo step) with the assistant as actor. After writing, the
   live model is diffed against the base again; any difference from what the person saw is
   reported.
5. **Conflicts.** If the live model changes while a plan is open, only the plan items that
   read or wrote the changed rows are marked and re-planned.

Safety limits: large envelopes need explicit confirmation and very large ones are refused;
outbound requests pass an egress policy (local addresses always allowed, any other host only
after per-project consent, or never in local-only mode); API keys are stored only in the OS
credential store or in process memory and cannot be printed or serialised. External agents
connect through the MCP server (loopback HTTP or stdio) and use the same tool catalog; their
writes wait in a draft for the same Apply.

## Scripting

Two script forms produce the same thing — an ops envelope:

- **Edit scripts (JSON).** An envelope file: title and groups of operations. The Scripts
  window adds, previews (dry run on a scratch copy) and runs them; a run is one undo step
  with the script named as actor. "Record actions to script" saves envelope edits made
  between start and stop.
- **Geometry scripts (Lua 5.4, sandboxed).** No files, network, clock or randomness; limits
  on CPU time, memory, executed lines and created elements. The same script and parameters
  give the same envelope and hash. The standard library covers vectors and transforms,
  units (`mm`, `cm`, `m`, `deg`, `len`, `angle`), declared parameters (`param`), catalog
  profiles, forms (`extrude`, sweeps, `loft`, revolves), patterns, `part`, `member`,
  `group`, `define_component`, `place`, booleans, `constrain`, `measure` and `audit`, plus
  parametric building blocks for frames, lattices and surfaces. Every created element has a
  stable key, so a rerun with new parameters becomes reshape/create/delete ops on that
  script's elements only. Run with `ddc run <project> <script.lua> --out <file>` or by the
  assistant.

## Extensibility

- **Importers.** Implement `ddc_io::Importer` (extensions plus `import` into a `Sink`) and
  register it in `ddc_io::Registry`.
- **Bridges.** Any external executable that speaks the bridge protocol
  ([docs/plugins/BRIDGE_PROTOCOL.md](plugins/BRIDGE_PROTOCOL.md)) can supply a format. Bridge
  packages are signed; the host verifies the manifest, runs a private verified copy with time
  limits and treats the stream as untrusted.
- **Edit language.** Tools outside the editor can write envelopes against
  `docs/schema/ops.v1.json` and run them as scripts or through MCP.
- **MCP.** External agents use the editor's tool catalog under the same plan/Apply rules.
- **Data packs.** Drawing standards (line weights, text heights, title blocks) and check rules
  are data, not code.
- **Commands and translations.** Commands and UI strings are JSON fragments embedded at build
  time (`crates/ui/data`).
