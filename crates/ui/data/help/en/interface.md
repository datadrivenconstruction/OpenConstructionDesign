# The editor interface
@tags interface, ribbon, project browser, properties, tabs, panels, layout
@context app.help panel.browser panel.properties view.tabs view.tile view.maximize view.close view.next view.goto view.sync_sel mgr.settings
@commands app.help panel.browser panel.properties panel.issues view.goto view.next view.close view.tabs view.tile view.sync_sel view.section_here mgr.settings
@see getting_started workspaces shortcuts command_palette

The editor window has a title bar (the project name opens the File menu, `Alt+F`), a command ribbon with a search field (`Ctrl+K`), a project browser, a canvas with views (plan, 3D, section, sheet), a properties panel and a status bar. The panel set depends on the chosen interface ([[topic:workspaces]]); any command can be found through the palette [[cmd:app.palette]].

Views open in tabs over the canvas. [[cmd:view.goto]] lists every view of the project (plans, 3D, sections, elevations, drafting views, sheets): type part of a name, `Enter` opens the highlighted one. [[cmd:view.next]] steps through the open tabs and back to the plan, [[cmd:view.close]] closes the open section or elevation tab. [[cmd:view.tabs]] keeps the tab strip with the plan and 3D tabs on top even when no section is open. [[cmd:view.tile]] (`WT`) shows the plan beside 3D, or beside the open section; with [[cmd:view.sync_sel]] on, a selection made in one of them is brought into view in the other, and a selection made in the browser, a table or the palette in both. [[cmd:view.section_here]] creates a section through the current viewpoint across the whole model and opens it: from the plan centre looking north, along the 3D camera, or across the open section; `Ctrl+Z` removes it. In the "Model + table" interface the schedule table sits next to the model and selection is shared with the plan.

**Folding the interface:** `Ctrl+B` folds or shows the project browser, `Ctrl+Alt+B` the properties, `Ctrl+Alt+I` the checks panel. A folded panel leaves a thin rail with its name: click it or rest the pointer on it to open the panel again; a double-click on a panel's edge folds it. `Ctrl+F1` switches the ribbon between full, tabs only and hidden; `Ctrl+Alt+O` and `Ctrl+Alt+U` show or hide the options bar and the status bar. `Ctrl+Shift+F1` leaves only the model view on screen; `Esc` or the same keys bring the interface back. `F11` maximises the window. The same commands are on the View tab.

## steps
1. The project browser shows the tree of levels, sections, elevations, sheets and schedules. Double-click opens a view in a tab.
   On a view or level row `Enter` opens it, `F2` renames it and `Del` deletes it (one undo step); the same commands are in the row's context menu and in the palette.
2. On the ribbon pick a tab ("Architecture", "Structure", "Forms", "Data", ...) and a tool. Selecting an element shows the "Modify | category" tab.
3. The properties panel shows the parameters and the type of the selection.
4. The status bar shows the hint for the active tool (steps and keys); typed numbers appear next to the cursor.
5. The status bar also shows the autosave state and the warnings counter.
6. Need a different panel set? Switch the interface, see [[topic:workspaces]].

## keys
- `F1` - help for the active tool or selection
- `Ctrl+K` - command palette
- `Ctrl+,` - settings

## tips
- Cannot remember where a command lives? Press `Ctrl+K` and type any word of its name.
- Language, theme and density settings are in [[cmd:mgr.settings]] (`Ctrl+,`).
