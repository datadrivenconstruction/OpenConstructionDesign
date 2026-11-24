# User guide

This guide covers the core workflows of release 1.1.0. It assumes the Windows desktop
editor; the `ddc` command line also runs on Linux. For a first model step by step, start with
[getting-started](user/getting-started.md).

The editor is a CAD-BIM tool: drawings, 3D geometry and building data live in one project.
Walls, rooms, ducts and structural members are elements with parameters; plans, sections,
sheets and schedules are views of the same model.

This guide covers the core workflows. Commands live on the ribbon tabs (Architecture,
Structure, Systems, Annotate, View, Data, Manage); any command can also be found with the
command palette (**Ctrl+K**). **F1** opens help. Press **Esc** twice to leave any tool.

## Projects and files

- **Ctrl+N** new project, **Ctrl+O** open, **Ctrl+S** save. A project is one `.ddc` file.
- Every edit is one step in the history: **Ctrl+Z** undo, **Ctrl+Y** redo, **Ctrl+Shift+H**
  history with the author of each change.
- **Data → Compare versions** shows what changed between two versions of a project.
- Autosave is configured under **Manage → Autosave**.

## Levels, grids and reference planes

1. **Architecture → Level** (or **Manage → New level**) adds a level by elevation. Rename or
   delete it with **Manage → Edit level / Delete level**.
2. **Ctrl+PageUp / Ctrl+PageDown** moves the plan view one level up or down; **Ctrl+1**
   opens the plan of the current level.
3. **Architecture → Grid** and **Reference plane** create datums that walls, columns and
   dimensions can snap and lock to.
4. **Manage → Coordinates and georeference** sets the project's survey position.

## Walls, floors, roofs and other building elements

- **Wall:** click points to draw a chain; type a length or angle while drawing to enter exact
  values. Snaps and alignment guides follow existing geometry. **Wall by room outline**
  creates walls around a closed outline. Wall types define layers; corners and T-joins are
  cleaned up automatically.
- **Door / Window / Wall opening:** pick a host wall and a position.
- **Floor / Ceiling / Roof:** sketch a boundary or pick walls. Roofs support slopes per edge;
  floors support sloped tops and shape editing. **Shaft opening** cuts through several floors.
- **Curtain wall:** draw like a wall, then edit the grid, panels and mullions.
- **Stair / Railing / Ramp**, **Room / Room separator / Zone**, and **Component** for
  furniture and equipment from the library (**Manage → Library / Load family**).
- Select an element to edit its parameters in **Properties** (**Ctrl+Alt+P**) or drag its
  grips. The **Modify** tab moves, copies, rotates, mirrors, arrays, offsets, trims, splits
  and aligns.

## Structural members

- **Structure** tab: **Beam**, **Structural column**, **Brace**, **Beam system**,
  **Structural wall**, **Structural floor**, **Column footing**, **Strip footing**,
  **Foundation slab**.
- **At grid intersections** places columns at every selected grid crossing.
- Beam ends join columns and other beams automatically; structural columns cut slabs.
- **Reinforcement** adds bar sets to columns, beams, walls, floors and footings; bars follow
  the host when it changes and keep stable marks.
- **Analytical model** shows and edits the analytical representation of members.

## MEP

- **Systems** tab: **Duct**, **Pipe**, **Cable tray**, **Conduit**, **Fitting and
  accessory**, **Equipment**, **Air terminal**, **Plumbing fixture**, **Electrical device**,
  **Light fixture**.
- Draw runs point by point; fittings (elbows, tees, crosses, transitions, caps) are derived
  from the network automatically.
- **Create system** groups runs and components; **Connect into** joins a run to another run
  or a component; **Auto-route** proposes a route; **Check system** reports open ends and
  inconsistencies.

## Views, annotation and sheets

- **View** tab: new plans, **Section**, **Compass elevations**, **3D view** (**Ctrl+3**),
  **Section box**, **Schedule**, visual styles, visibility and graphics, view templates.
- **Annotate** tab: linear, angular and radial dimensions, dimension chains along walls,
  spot elevations, text, tags (**Tag all untagged**), leaders, detail lines and components,
  filled regions, revision clouds, legends.
- **View → New sheet** creates a sheet with a title block; **Place view on sheet** adds plans,
  sections and schedules at scale.
- **Data → Export sheets to PDF/SVG** writes vector output with real line weights.

## IFC and other exchange

- **Open an IFC file** with **Ctrl+O**: IFC2X3, IFC4 and IFC4.3 (and `.ifczip`) become native
  elements where recognised and meshes with properties otherwise. Re-importing the same
  source updates elements instead of duplicating them.
- **Data → Export IFC / IFC2x3 / IFC4 / IFC4.3** writes the model in the chosen schema.
- **Manage → Import DWG/DXF** places a `.dwg` or `.dxf` drawing as an underlay or converts it
  to model elements; **PDF underlay** places PDF pages at a given scale on a level.
- **Data → Export / Import XLSX** round-trips element parameters through a spreadsheet; the
  import is one undo step with a per-cell report.
- **Data → Export glTF / STL / OBJ / STEP / 3MF** writes geometry. BCF issue files can be
  imported and exported from the issues panel.
- **File → Import** lists formats provided by installed bridges (for example `.rvt`).
- **Manage → Linked models** links other project files for reference.

## Checks and coordination

- **Data → Check against rules** runs model checks; each finding links to the element and,
  where possible, offers a fix.
- **Data → Find clashes** compares two sets of elements (by category, level or selection)
  for hard clashes or clearance, with statuses and comments stored in the project.
- **Data → Query** and **Open as table** select and edit elements by their data.

## AI assistant

1. Configure a connection in **Settings**: address of a compatible model endpoint (a local
   server or a remote service), protocol and key. Keys go to the system's secure store.
   **Local only** keeps every request on this computer and local network; a remote address
   needs consent per project before the first request.
2. Open the assistant with **Ctrl+J** (**Ask AI**), or use **Explain selection** and **Fix
   issues with AI**.
3. The assistant can answer questions about the model, or propose changes. A proposal is
   prepared on a copy of the model and shown as a plan card with the exact list of created,
   changed and deleted elements, and as a draft in the views.
4. Press **Enter** (Apply) to write the proposal, or **Esc** to discard it. Hold **B** to
   see the model as it is before the change.
5. An applied proposal is one undo step and is marked as an AI change in the history.

External MCP clients can connect through the built-in MCP server; their changes wait for the same
Apply.

## Scripting

- **Manage → Scripts and nodes** manages edit scripts (JSON). **Preview** shows what a
  script would create, change and delete; **Run** applies it as one undo step.
- **Manage → Record actions to script**: start, edit, stop. The recorded edits become a
  reusable script.
- Geometry scripts in Lua run in a sandbox, either through the assistant or from the command
  line:

  ```sh
  ddc run project.ddc canopy.lua --out canopy.ddc --param span="24 m" --level "Level 1"
  ```

  The result is written to a new file; the input project is never changed. See
  [ARCHITECTURE.md](ARCHITECTURE.md#scripting) for the script API.

## Command line

```sh
ddc info model.ifc --json                      # counts by category, levels, warnings
ddc open model.ifc --export xlsx out.xlsx      # convert without opening the editor
ddc batch jobs.json                            # many independent jobs
```

`ddc --help` lists all commands, export formats and exit codes.

## Where to go next

- [docs/user/ai-assistant.md](user/ai-assistant.md): providers, preview and Apply.
- [docs/user/formats.md](user/formats.md): supported formats and the IFC round trip.
- [docs/user/families.md](user/families.md): parametric families.
- [ARCHITECTURE.md](ARCHITECTURE.md): how the editor works inside.
- Problems or ideas: open an issue; vulnerabilities go through [SECURITY.md](../SECURITY.md).
