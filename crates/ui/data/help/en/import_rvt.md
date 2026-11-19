# Importing .rvt and .rfa
@tags rvt, rfa, import, open, pro BIM tools
@commands file.open mgr.links mgr.purge
@see import_ifc linked_models big_files

`.rvt` (model) and `.rfa` (family) files open through the "Import" window with the separate signed `.rvt` / `.rfa` module. If the module is not installed, the window says so and the format stays unavailable.

## steps
1. Choose Open (`Ctrl+O`, [[cmd:file.open]]) and pick an `.rvt` file.
2. The "Import" window shows whether the module is available (".rvt/.rfa - available" or the reason it is not).
3. The load runs in the background: the interface stays usable, the progress bar and the "Cancel" button are in the "Import" window. Only one import runs at a time.
4. Inspect the result in 3D and compare with the source. Issues are collected in the warnings window ([[topic:warnings]]).
5. Saving as an editor project (`Ctrl+Shift+S`) is not wired in this build; autosave protects your work ([[topic:autosave]]).
6. `.rfa` families are read by the same module and appear in the library: [[topic:families]].

## keys
- `Ctrl+O` - open
- "Cancel" in the "Import" window stops the load

## tips
- Attaching the source as a linked model is planned ([[topic:linked_models]]).
- For large files use a section box and isolation ([[topic:big_files]]).
- Unsupported content is not lost silently: it is listed in the warnings.
