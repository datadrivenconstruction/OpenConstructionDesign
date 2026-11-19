# Files and projects
@tags file, project, new, open, purge
@context file.new file.open file.save file.save_as mgr.project_info mgr.purge
@commands file.new file.open file.save file.save_as mgr.project_info mgr.purge
@see autosave import_ifc export

A project is a single database file. Here are the main file operations and project housekeeping commands.

**Not available yet:** new project (`Ctrl+N`), save (`Ctrl+S`) and save as (`Ctrl+Shift+S`). The ribbon buttons of these commands are inactive.

## steps
1. The start screen offers "New model" (start with a wall), "Open..." and "Import" and a list of recent files. `Ctrl+O` ([[cmd:file.open]]) and the menu "File > Import..." open the "Import" window: `.ifc` / `.ifczip`, `.dxf` / `.dwg`, `.pdf`, `.xlsx`, `.rvt` / `.rfa`.
2. Until saving to a file is wired, your work is protected by autosave snapshots: [[topic:autosave]].
3. Project information (name, address, client, organization, signatures, stage) - [[cmd:mgr.project_info]].
4. Cleanup: [[cmd:mgr.purge]] (`PU`) removes unused types and families; one undo brings them back.
5. To exchange with other programs - [[topic:export]]; for foreign files - [[topic:import_ifc]], [[topic:import_rvt]].

## keys
- `Ctrl+O` - open / import
- `Ctrl+Z` - undo

## tips
- A project is one database file; keep copies separately.
- The sample house: [[cmd:help.sample]].
