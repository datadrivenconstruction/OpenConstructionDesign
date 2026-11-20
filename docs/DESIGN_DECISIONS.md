# Design decisions

The key architecture decisions behind OpenConstructionDesign, each with its rationale.

## 1. Rust for the whole core

**Decision.** The model, geometry, solver, import/export, renderer and application are written
in Rust as one Cargo workspace.

**Rationale.** Performance and memory safety without a garbage collector, safe parallelism for
regeneration and import, and one language that targets desktop and WebAssembly.

## 2. The model as an operation journal

**Decision.** Every change is an operation recorded in a journal with its author, title and
inverse. The current state is kept in tables; the journal holds what is needed to undo each
operation and, for structured edits, the forward operations.

**Rationale.** Undo, history, provenance, version comparison and merging come from one
mechanism instead of several. Storing inverse SQL makes undo uniform across all tables.

## 3. SQLite as the native file format

**Decision.** A project is a single SQLite database: element, parameter and journal tables, with
geometry where needed.

**Rationale.** The format is open and can be read by any SQLite tool; data is reachable by SQL
and spreadsheets; transactions, savepoints and online backup come for free.

## 4. Open core; closed formats outside it

**Decision.** The editor and all open-format support are open source. Readers for closed
formats are not part of the repository and connect only through public interfaces.

**Rationale.** The open code base contains no knowledge of closed formats, builds and tests
without them, and third parties can provide their own format support through the same
interfaces.

## 5. Closed formats through a separate bridge process

**Decision.** A closed format such as `.rvt` is read by a separate, signed executable that
streams neutral building data (levels, types, walls, openings, slabs, rooms, meshes,
properties) over a versioned, documented byte protocol. The host verifies the package
manifest and signature, runs a private verified copy with time limits and cancellation, treats
the stream as untrusted, and maps it into native elements in one undoable operation.

**Rationale.** No linking between code under different licences; a crash or hang of the
bridge never takes down the editor or changes the model; independent release cycles; a stable
byte protocol instead of an unstable binary interface. The cost, copying data through a pipe,
is not a bottleneck.

## 6. Desktop first, WebAssembly-compatible core

**Decision.** The desktop editor is the primary product. Model, geometry and solver crates use
no APIs that are unavailable on wasm32; clock access goes through a crate that works in both
environments, and a model can be loaded from and saved to bytes.

**Rationale.** Desktop gives the performance and file access professional work needs, while
keeping the core portable avoids a second code base later.

## 7. Own renderer on wgpu, egui behind an own UI layer

**Decision.** The 2D/3D viewport is a custom wgpu renderer (instancing, culling, GPU picking,
on-demand rendering). Panels and tools use egui on the same GPU context, wrapped by an in-house
shell kit (design tokens, icons, commands, themes, localisation); application code depends on
that kit.

**Rationale.** Full control of 3D performance with fast UI development now, and the option to
replace the immediate-mode layer later without rewriting the application.

## 8. Own geometry kernel, permissive dependencies only

**Decision.** A Rust BIM kernel for parametric elements: planar faces, extrusions, arcs,
sweeps, exact predicates, booleans for walls, openings and slabs, sections,
with NURBS and STEP support built in-house. Only permissively licensed dependencies are
allowed.

**Rationale.** Control over speed and stability on large models, one kernel for desktop and
WebAssembly, and a licence-clean dependency tree.

## 9. 2D and 3D in one document

**Decision.** 2D primitives are elements of the same model; plans, sections, sheets and 3D
views are windows into one database. `.dwg`, `.dxf` and PDF are first-class inputs, and
drawings can be converted to model elements by rules.

**Rationale.** No synchronisation between separate drawing and model files; drawings stay
consistent with the model by construction.

## 10. Imported data becomes native

**Decision.** Recognised elements from IFC and bridge imports become native parametric objects;
the rest is kept as meshes with category and properties. Stable external references make a
re-import update existing elements instead of duplicating them.

**Rationale.** Imported models can be edited with the same tools as native ones, and repeated
exchange does not degrade the model.

## 11. Standards and checks as data

**Decision.** Drawing standards (line weights, text heights, dimensions, title blocks) are data
packs selected by profile and scale. Check rules are declarative data; the verdict is computed
deterministically by code, and each finding links to its element and, where possible, a fix.
Metric and imperial units are equal citizens.

**Rationale.** New regions and company rules need data, not code changes; results are
reproducible and explainable.

## 12. One serialisable edit language

**Decision.** UI tools, spreadsheet import, scripts, the AI assistant and external MCP clients
produce the same typed, versioned edit envelope. References are stable identifiers, every
quantity names its unit, unknown fields are rejected, and older versions are upgraded before
use. One envelope is one journal entry and one undo step.

**Rationale.** One executor and one validation path for every source of change; edits can be
recorded, replayed, diffed and audited.

## 13. The AI assistant proposes; a person applies

**Decision.** The assistant works on a scratch copy of the model and never writes to the live
model. The plan card is built from a row-level database diff, not from the assistant's own
description. Only a person's Apply writes the change, as one undo step attributed to the
assistant, and the result is verified against what was shown.

**Rationale.** The person sees exactly what will change before it happens, mistakes cost one
undo, and the history shows which changes came from the assistant.

## 14. One interaction model for all tools

**Decision.** A single interaction state owns the input. Tools are sessions that read the
editor, draw previews and return a record on commit; one write path applies it as one undo
step. Input is view-independent (a ray, a work plane and a scale), so the same tool works in
plan, section and 3D. Refusals are typed codes with suggested fixes. Interactions are
described in a text scenario format used by tests.

**Rationale.** One owner of "what is happening now" replaces scattered flags; behaviour is
reproducible from scenarios, and the same scenario in plan and 3D must produce the same
journal.

## 15. Deterministic, sandboxed scripts and generators

**Decision.** Geometry scripts run in a sandbox without files, network, clock or randomness,
with limits on CPU time, memory, operations and elements. Generators are pure and
seed-deterministic. Both output edit envelopes.

**Rationale.** The same input gives the same model, so results can be cached, compared,
re-run with new parameters and trusted not to touch anything outside the model.
