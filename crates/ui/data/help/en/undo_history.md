# Undo and history
@tags undo, redo, history, journal
@context edit.undo edit.redo app.history
@commands edit.undo edit.redo app.history
@see autosave modify

Every action is written to the model journal: one gesture, one entry. Undo and redo are unlimited in steps, and committed work is never lost.

**Not available yet:** the history window (`Ctrl+Shift+H`) with going back to a chosen state. The ribbon buttons of these commands are inactive.

## steps
1. `Ctrl+Z` ([[cmd:edit.undo]]) - undo; `Ctrl+Y` or `Ctrl+Shift+Z` ([[cmd:edit.redo]]) - redo.
2. Inside an unfinished gesture `Ctrl+Z` drops the whole draft (the entered points), the journal is untouched; `Backspace` removes only the typed digit.
3. When nothing is left to undo the status line says so.
4. Each completed operation is one undo step, including bulk ones.
5. An `.xlsx` import, a type change and a query with `->` undo in a single step.
6. Saving and autosave: [[topic:autosave]].

## keys
- `Ctrl+Z` - undo, `Ctrl+Y` / `Ctrl+Shift+Z` - redo
- `Backspace` - erase the typed digit (never deletes elements)

## tips
- A new action after undo discards the redo branch.
- Anything deleted can be recovered by undo; the editor never deletes your files itself.
