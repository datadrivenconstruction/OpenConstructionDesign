# Changelog

## 1.4.0 (unreleased)

### Command line
- Release builds include `ddc gen` and `ddc publish`; the ERP link builds without the GUI.
- `ddc new` creates an empty project; `ddc run --new` runs a script without an input file.
- `ddc run` applies large scripts in linear time and prints `applying N/total`.
- `ddc ai` runs the AI assistant without a window, with a cost estimate and `--budget-usd`.
- `ddc ai-profile add|list|remove` manages the same AI profiles as the app; keys stay in the OS keychain.
- Usage errors are one line with a hint instead of the whole help.
- An option never takes the next `--flag` as its value; `ddc --json` without a command returns a JSON error.
- `ddc run` reports level read errors instead of hiding them.

### Scripting
- Lua builds lattice girders, double-angle members, shells and roofs as model elements.
- Lua `member{}` and `slab{}` blocks.
- The lattice audit joins members crossing at a node instead of reporting clashes, and names ids and points.

### Video and frames
- `--reveal-out` always runs headless; `--reveal all` includes script-made elements, built bottom-up.
- Camera choice for reveals: `fixed`, `orbit`, `follow`.
- Sections and plans work for models made only of members; screenshots show only the canvas.
- One language rule everywhere: `--lang`, then the setting, then the system language.
- UI shot scripts can open another project between steps and keep the view, and set the 3D camera with `zoom`, `eye` and `target`.

### AI assistant
- Cost uses the amount the service reports for each reply, and cached input is counted at its own price; long chats reuse the cached start of the conversation.
- Model gateway preset: models, prices and context size come from the gateway.
- A spent key limit and an empty balance are their own errors with a next step; the balance is shown.
- Mode switch, onboarding, recovery and warning texts explain what each choice does.
- A reply cut at the output limit continues on its own; the output cap follows the model's maximum.
- The status tells local work apart from a silent model service.
- Plan limits with an unlimited mode, chosen from a chip above the input; `ddc ai --apply` needs `--apply-partial` to apply a plan cut by a limit.
- The chat shows the running cost and the cost of each step.
- Time and token limits are shown and adjustable; `ddc ai --max-tokens`, `--max-time-s`; a stop names the limit and how to raise it.

### 3D view
- Shaded and realistic styles share one lighting curve; saturated colours keep their hue, the sky follows the backdrop and outlines scale with the frame.
- Solids and freeform bodies show their material colour; glass is see-through, while steel and concrete keep their texture.
- Stone, water and grass finishes; naming a new material on a solid adds it to the material list.
- Reveal shows solids and masses and starts in 3D; --reveal-from, --reveal-to and --reveal-step pick the frames.
- Orbit around the model centre, or let the view orbit on its own.
- A large AI plan replays in the journal element by element.
- Corner shading follows depth, without halos around edges.

### Materials
- Ten new textures: stone, gravel, soil, asphalt, plaster, roof tile, insulation, water, steel plate and rubber.
- The materials list, material card and properties show a swatch of each material with its hatch.
- Plans and sections share one hatch per material class, with steel, earth, water, stone, stipple and glass patterns.
- Colour the model by material, with a legend.

### Export
- glTF exports carry metal, roughness and refraction per material; glass is smooth and refractive.
- IFC exports carry surface roughness and a reflectance method for each material.

### Editor
- Realism checks flag solid glass, filled blocks, parts of unlikely size and parts outside the outline.
- Solids appear in the spreadsheet and CSV exports with volume, area and net volume, and in OBJ and mesh exports with unique part names.
- The window is named OpenConstructionDesign.
- Scrollbars never cover content.
- The model summary counts members, plates and other solids.
- Realism checks of steel joints scale to large models.
- Steel gusset, end plate and base plate bolts fit the connected section: heads and nuts stay clear of sloped angle legs and leaning columns, and joints that would fail the check are not placed.
- Every locale has all interface texts.
- Materials set by scripts render in 3D; script-made parts are listed in the project browser.
- Geometry and export errors name the element and the next step; the SVG export explains what it includes.
- Disabled buttons say why; empty parameter, size and import-rule tables say how to start.

## 1.3.0

### Languages
- The interface comes in 45 languages. English is the source text; every other language is proofread and kept in its own folder, and regional variants keep only their differences.
- The AI assistant answers, names things, writes sheet words and gives refusals in the language of the interface, and creates project data (names, notes, schedules) in the user's language.
- Each project has a document language: drawings, schedules and exports use it, independent of the interface language.
- Generated service, circuit and air-system names come from the language bundles.
- Defaults are locale-neutral: phases, import words and CSV decimals follow the locale; ISO sheet words stay fixed in every document language.
- Right-to-left text is shaped in widgets and ribbon labels.

### Interface
- Interface modes: ribbon, rail and minimal. The mode is chosen at onboarding and can be changed later; the ribbon can be folded.
- Ctrl+K opens the command palette.
- A new update notice: a badge in the tab row and a warning banner that spans the dialog width.
- Icons take their tint from the theme; ribbon polish.
- In 3D, an armed tool owns the clicks of the view, and the empty-view card and navigation hint stay out of the way while a tool is live.
- Scrollbars keep their own gutter, so fields and values never sit under the bar; tables keep their columns clear of it.
- Undo and redo have new icons.
- Library cards show two-line names, ribbon captions are taller, and Settings scrolls.
- The rail and the context bar clear the view tabs; the bar follows the selection.

### Norms and units
- The starter norm pack is country-neutral; regional packs are chosen by the user.
- The project currency is an ISO 4217 code.
- CSV decimal style is named by the decimal mark.

### AI assistant
- The plan card shows the area and cost change of a plan, and the plan check joins the critic loop with a self-check on the card.
- Receipts roll back to the state before any apply.
- Compliance answers cite the rule, clause and elements.
- Answer mode chip and starter cards in the composer; follow-up chips sit one per row, sized to their words.
- Large plans are gated by the number of changed elements.
- `connect_members` picks the type automatically; `section_plane` has a command contract.
- Rolling a plan back is one step with one regeneration, and runs of steel connections apply in one batch: large AI plans finish much faster.
- Critic notes stay inside a JSON tool reply.
- AI snapshots and report shots render the canvas only, without the interface around it.

### Generator
- A built-in preset keeps its base names through JSON.
- HVAC: zone cuts and corridor niche shafts for dense zones; a compact shaft turns its riser tee and feeds the main by a long branch; a dedicated system with an over-velocity run is not taken.
- Hospital supply velocity checks run again.
- `plan_dump --metrics` prints service counts and generator metrics.
- Electrical boards sit low enough for their drop under any ceiling; branch tees keep the spacing their fittings need.

### Rendering
- Offscreen 3D works on the GL backend, so headless frames render on machines without a GPU driver for the default backend.

### Command line and docs
- A usage error is one line with a hint instead of the whole help.
- README lists only the recipes in the repository, and CI checks every path it names.
- `ddc-app --help` and docs/user/video-and-frames.md describe the video and frame flags.

### Browser version
- The full editor builds for the browser (wasm32) and installs into the ERP from a signed release feed.
- Session diff reads the base copy through the store.

### Project server link
- Every page of the revision list is read; a lost lock is recognised by its code.

## 1.2.0

### Editor
- The roof tool works in 3D: sketch an outline or pick walls on the level plane.
- In 3D, wall tops and bases attach to a roof or floor by picking, and join, unjoin and cut work on picked elements.
- The plan view range tops out at the next level by default; components show by their body span.
- The plan HUD chip shows the view scale and the zoom apart.
- Imported models get default views.
- The AI chat has one composer surface, signed replies and quieter chrome.

### Import and exchange
- IFC import subtracts bounded half-spaces instead of skipping them, and room names are no longer doubled.
- IFC export writes the analysis model only from the project's own analysis input.
- `DDC_Provenance` is written to the props of elements.parquet.
- The headless `ddc` command line builds again with the link feature; headless runs skip sheet outputs when there are no sheets.

### Generator
- A flat builds every room its type asks for; the residential tower brief is 61 m wide so the full flat mix fits.
- Base programmes and typology traits come from data; the code branches on traits, not on named building types.
- Plate layouts: basements follow the core of the plate, bands narrow no further than the stair band, walls where wings join are inner walls.

### AI assistant
- The assistant composes structures from building blocks and techniques; the block reference and scripts arrive whole.
- AI project turns finish well within the wait.

## 1.1.1

### Editor
- Tab with a plan tool armed keeps the focus on the canvas.
- The first click on the plan goes through even while the start card is still shown.
- The New sheet dialog fits small screens: its fields scroll, Cancel and OK stay visible.

## 1.1.0

### Editor
- Plan tools work in the 3D view; a temporary section plane cuts the model in 3D.
- Duct, pipe and cable tray runs are drawn directly in 3D.
- Tab over the 3D view cycles the elements under the cursor and no longer jumps to the ribbon.
- The Realistic style compiles again and shows materials; the status bar says when the Speed profile turns textures off.
- The empty plan suggests any modelling tool or the Generate chat, not only walls.
- Headless scripted UI shots: `ddc-app --ui-shot script.txt`.

### Drawings and sheets
- The drawing set builds plans, sections and elevations for any model, also one without walls (bridges, plants, forms).
- Sheet and export plans draw mesh elements (facades, glazing, grouped bodies).
- Viewports with their dimensions and tags stay off the title block.
- Sections cut bodies whose face lies exactly on the cut plane.

### Import and exchange
- DWG/DXF underlays are kept when a headless run saves the project, and the reopen check compares them.
- IFC import turns triangulated walls, slabs and roofs into native elements.
- IFC export keeps the class and quantities of mesh elements.
- Openings flush with the wall end are accepted within the length tolerance.

### Generator and MEP
- Process plant: every nozzle is piped and routing no longer hangs.
- Power feeds start at the parent's tee; the riser stops at the top built board.
- Courtyard school briefs build instead of refusing; plant hall, light industry, dampers, slab holes and chase bridges fixed.

### AI
- Every AI tool limit lives in one registry; each tool description states its limits, so the assistant plans around them.
- Truncated tool replies say "shown N of M" and the next offset instead of cutting silently.
- User-facing limits are adjustable in Settings, each with the reason it exists.
- A turn that times out says what the session still waits on.

## 1.0.0

### Generator
- Free-form plates: apartment plates with curved facades and turned terraces; the core holds the whole lift bank.
- Apartment layout: every window room keeps a door onto its hall; the inner zone deepens until each room is reached.
- Natural planting: tree groups, shrub drifts, botanical species and plant symbols.
- Near trees: leaf-card crowns, branched skeletons and screen-size detail levels.
- Hospital wing, operating theatre block, secondary school and conference hotel briefs.
- Structural system from the brief (concrete, timber, masonry); long spans on their own columns.
- Footbridge ramps with landings, cable-stayed bridge pylons, lattice tower bracing, perforated screens and fins with fixings.
- Data centre unit seating step is a rule (dc.unit_step).

### Modelling
- Material look library: tinted layers with roughness, metallic and bump; one assignment rule for 3D and IFC.
- Mesh boolean operations are faster and more robust; Cancel stops them without a fallback.
- Arc walls regenerate locally and take a top profile; walls under dormers and mono-pitch roofs.
- Imperial types for the hedge, lawn and shrub families; scale is an instance parameter.
- Norm packs state their completeness and list gaps against the reference pack.
- Full regeneration writes only the wall quantities that change.

### MEP
- Balanced ventilation: zoned air systems, supply and extract balance, large air handling units.
- Electrical in the generator: every room of the apartment section and the school is powered.
- Electrical circuits: wiring on plans, cable routes in 3D and in IFC.
- Water and drainage reach every fixture in all typologies.
- Duct and pipe routing: tees between lattice lines, clear of port straights, elbows fitted to short legs.
- Hood ducts tee into their extract branch; risers to rooftop units stay in the roof equipment zone.

### Import and export
- IFC: equipment, lift, column and beam families get their own type objects; a removed element takes its property sets.
- XLSX: rows created by an earlier Apply are not key-matched to their own element; the import report lists annotations a deletion removes.
- Plan image import: door jambs and swings keep faces whole; the reported drawing angle is in the model frame.
- Parquet export moves to parquet 60.1.
- Project parameter values set after an import are local edits.

### AI assistant
- Tool arguments are checked against the schema before a tool runs.
- Write receipts and a guard against mass deletion; set_material tool and look library listing.
- Edits made while a turn runs start their replan after the turn.
- Refusals use one typed contract.

### User interface
- Heavy edits and view rebuilds run in the background; Esc cancels them.
- Ribbon icon set with six pickable styles, new glyphs for STEP, 3MF, glTF, STL and OBJ export.
- Layout fixes: long labels wrap, view bars scroll sideways, icon buttons are named for assistive technology.
- README showcase with generated buildings.

### Build
- Faster build profile and merged test binaries.
- Tagged release builds for Windows and Linux.
- Dependency advisories cleared before release.

## 0.9.0 - first public release

Pre-1.0: the project file format and the interface can still change between releases. Keep an
IFC export of every model you care about.

### Editor
- Levels, grids, walls, floors, roofs, stairs, openings, doors, windows, structure, MEP
  systems and rooms, in plan, section, elevation and 3D views.
- Every edit is a journaled step: undo, redo, and a history that shows who changed what.
- A project is one `.ddc` file holding the model and its full change history. Compare two
  versions to see what differs.
- Parametric families: components with types and parameters defined inside the model, edited
  in bulk and shown in schedules.
- Command search (Ctrl+K), a sample model to start from, and a safe-graphics start mode for
  machines where the window does not open normally.

### AI assistant
- Describe a change in words. The assistant plans on a scratch copy, then shows a plan card,
  a preview and a diff. Nothing reaches your model until you press Apply; an applied plan is
  one undo step.
- Works with the provider of your choice (any compatible HTTP chat interface, or a server on
  your own machine). You pay your provider directly; the editor runs no AI service. The key
  is kept in the Windows key store.

### Files and exchange
- Open IFC (IFC2X3, IFC4, IFC4X1 to IFC4X3, `.ifczip`) and `.dwg`/`.dxf`. The original file is
  never modified.
- Export IFC2X3, IFC4 and IFC4.3, XLSX (parameter round trip), BCF 2.1 and 3.0, glTF, STEP,
  STL, OBJ and 3MF; sheets export to PDF and SVG.
- `.rvt` files open only through an optional bridge module distributed separately.

### Documentation and checks
- Sheets with title blocks, schedules, plans, sections and elevations.
- Clash detection, rule checks and model integrity checks.
- JSON edit scripts, sandboxed Lua geometry scripts and the headless `ddc` command-line tool
  for batch conversion.

### Installer
- Windows 10 or 11 on x86_64. Installs for the current user without administrator rights by
  default, adds a Start menu entry, and offers optional `.ddc` and `.ifc` file associations.
- The installer is not code-signed; verify the download against the `.sha256` file published
  next to it.

### Known limits
- The `.dwg` decoder is an independent implementation and does not cover every feature;
  compare imported models with the source.
- The AI assistant can make mistakes. Read the preview and the diff before you apply.
- The structural solver is not yet available in the editor window.
- Very large models can be slow.
- English is the source language; the interface ships in 45 languages.
- Linux and macOS: only the libraries and the command-line tool are supported.
