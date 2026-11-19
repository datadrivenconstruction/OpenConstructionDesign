# Getting started

## Open or create a model

- **Ctrl+N** creates a project, **Ctrl+O** opens a file, **Ctrl+S** saves. A project is one
  `.ddc` file (SQLite) holding the model and its full change history.
- You can open IFC, `.dwg` and `.dxf` files directly (`.rvt` needs an optional bridge module
  distributed separately, see [formats.md](formats.md)). The editor converts them into a
  project; the original file is never modified.
- Save as a project, or export to IFC, XLSX, glTF and other open formats (see
  [formats.md](formats.md)).

## First model in ten minutes

1. Add a level (Architecture, Level) and open its plan (Ctrl+1).
2. Draw walls with the Wall tool: click points, or type a length and angle.
3. Add a floor, a door and a window; they attach to the wall that hosts them.
4. Switch to the 3D view and orbit with the mouse.
5. Press Ctrl+K and type a command name to find any tool.

## Undo and history

Every edit is one journaled step. Ctrl+Z and Ctrl+Y undo and redo; Ctrl+Shift+H shows the
history with the author of each change. Data, Compare versions shows what differs between two
versions.

## Documentation views

Plans, sections, elevations and schedules are views of the model. Place them on sheets with a
title block and export sheets to PDF or SVG.

## If something looks wrong

Start the desktop editor with `--safe-graphics`. Report bugs through the issue tracker with the
version, platform and, if you may share it, a minimal file.

More: [AI assistant](ai-assistant.md), [families](families.md),
[full user guide](../USER_GUIDE.md).
