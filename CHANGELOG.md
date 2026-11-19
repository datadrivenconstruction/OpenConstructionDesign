# Changelog

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
- Interface languages: English and Russian; other languages are not yet translated.
- Linux and macOS: only the libraries and the command-line tool are supported.
