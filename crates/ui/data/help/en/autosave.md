# Autosave and recovery
@tags autosave, recovery, save, crash, backup
@context mgr.autosave
@commands mgr.autosave file.save file.save_as app.history
@see undo_history files_projects

The project is stored in an SQLite database and every operation is written to the journal. Autosave periodically fixes the state; after a crash the editor offers to restore your work.

## steps
1. Open `AU` ([[cmd:mgr.autosave]]): the window has autosave on/off, the interval, "After N changes" (0 - by time only), how many snapshots to keep, the folder, "Remind to save" and "Snapshot now".
2. Saving to a project file (`Ctrl+S`, `Ctrl+Shift+S`) is not wired in this build yet; before a risky step press "Snapshot now" in the autosave window.
3. The status bar shows the autosave state: "Saved 12:04", "Autosaving...", "No changes", "Autosave off", or a warning if writing failed; click it to open the settings.
4. After an abnormal exit the next start shows the "Recovery" dialog with the newest snapshot: file, snapshot time, size.
5. Choose "Restore", "Open original" or "Compare" (added, removed and changed rows per table). A damaged snapshot is skipped.

## keys
- `AU` - autosave settings

## tips
- Snapshots are written in the background and do not freeze the editor.
- Before a risky bulk edit press "Snapshot now".
