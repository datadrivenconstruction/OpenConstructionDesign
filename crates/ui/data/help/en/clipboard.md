# Clipboard
@tags clipboard, copy, paste, cut
@context edit.cut edit.copy edit.paste edit.paste_aligned mod.cut mod.copy_clip mod.paste
@commands edit.cut edit.copy edit.paste edit.paste_aligned mod.cut mod.copy_clip mod.paste
@see modify selection

Elements can be copied to the clipboard and pasted into other views, levels and projects. Paste keeps relative positions.

**Status: planned.** Cut and paste (the clipboard is wired only for table cells) is not available in this build yet: the ribbon button is inactive. The steps below describe the intended workflow.

## steps
1. Select elements.
2. Copy - `Ctrl+C` ([[cmd:edit.copy]]); cut - `Ctrl+X` ([[cmd:edit.cut]]).
3. Paste - `Ctrl+V` ([[cmd:edit.paste]]): the preview follows the cursor, click the place.
4. Paste aligned - `Ctrl+Shift+V` ([[cmd:edit.paste_aligned]]): handy for copying to another level.
5. The ribbon commands [[cmd:mod.copy_clip]], [[cmd:mod.cut]], [[cmd:mod.paste]] do the same.
6. After pasting, check joints and level bindings.

## keys
- `Ctrl+C` `Ctrl+X` `Ctrl+V`
- `Ctrl+Shift+V` - paste aligned
- `Ctrl+Z` - undo

## tips
- For a copy within a view `CO` or `Ctrl`+drag is simpler ([[topic:modify]]).
- Pasting to another level keeps the layout, heights come from the level.
- A paste is one history entry.
