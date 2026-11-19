# Scripts and action recording
@tags script, automation, macro, nodes, record
@context mgr.scripts mgr.record
@commands mgr.scripts mgr.record data.query
@see query_bulk command_palette

A script is an edit file: a JSON envelope in the edit language the tools, the XLSX import and the assistant use. It has a title and groups of operations (create, change, delete). Scripts live in the scripts folder of the settings directory; the window shows the folder path.

## steps
1. Press `SR` ([[cmd:mgr.scripts]]) - the scripts of the folder are listed by name.
2. To add one press "Add script" and pick a `.json` file: it is checked and copied to the folder (an existing script is never overwritten).
3. Pick a script: its title and its groups with the number of operations are shown; a file that cannot be read says why.
4. "Preview" is a dry run on a copy of the model: how many elements the script would create, change and delete. The model is not touched.
5. "Run" applies the script; if any operation is refused, nothing changes and the reason is shown.
6. Bulk edits by condition are simpler with a query: [[topic:query_bulk]].

## keys
- `SR` - scripts
- `Ctrl+Z` - undo the last script run
- `Ctrl+K` - palette

## tips
- One script run is one history entry; `Ctrl+Z` reverts it whole.
- The change history names the script as the author of its changes.
- Unknown fields and envelopes from a newer version are refused, so a typo never runs half a script.
