# Warnings and checks
@tags warnings, errors, checks, issues, status
@context view.warnings mgr.warnings panel.issues
@commands view.warnings mgr.warnings panel.issues data.check
@see checks_compare clash import_ifc

The editor does not bury you in modal dialogs: problems are collected in the warnings window, and the status bar shows the total - ✓, ⚠ N or ✖ N.

**Not available yet:** the checks panel (`Ctrl+Alt+I`), `WW` and the rules (`data.check`). The ribbon buttons of these commands are inactive.

## steps
1. Click the check counter in the status bar or open [[cmd:view.warnings]] (`WG`).
2. The list is grouped by type: unclosed rooms, orphaned bindings, import warnings, etc.
3. Click an entry - the problem element is selected and the view jumps to it.
4. Fix the cause; the entry disappears after the recompute.
5. Before issuing, make sure the counter is clear or the remarks are deliberate.

## keys
- `WG` - warnings window
- `Esc` - close the window

## tips
- Each message says what happened, why and what to do.
- The command-line message feed shows recent events ("Created 4 walls, joined 3 corners").
- IFC and `.rvt` import warnings: [[topic:import_ifc]].
