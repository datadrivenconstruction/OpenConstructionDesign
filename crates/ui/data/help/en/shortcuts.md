# Shortcuts and mnemonics
@tags shortcuts, mnemonics, keymap, hotkeys, layout
@context mgr.keymap app.repeat
@commands mgr.keymap app.repeat app.where_is
@see command_palette interface

Commands are invoked by two Latin letters typed in a row (mnemonic) or `Ctrl` combinations. Single letters never launch commands, so there are no conflicts.

**Not available yet:** repeat last command (`Enter` / `Space`), and the key map window (`Ctrl+Alt+K`). The ribbon buttons of these commands are inactive.

## steps
1. Focus a view and type two letters: `WA` - wall, `DO` - door, `MV` - move. The timeout between letters is 0.8 s.
2. On other keyboard layouts the same physical keys work.
3. The first letter is echoed in the status bar; inside a command single keys are step options (`Tab`, `Space`, `C`, `A`).
4. Ctrl combinations: `Ctrl+Z` undo, `Ctrl+Y` redo, `Ctrl+K` palette, `Ctrl+E` export to XLSX, `Ctrl+A` select all, `Ctrl+3` 3D, `Ctrl+0` fit.
5. Not every declared shortcut is wired yet: if a command is unavailable, its ribbon button is inactive and the key shows the toast "not available yet".
6. Need a command but forgot the key? [[topic:command_palette]].

## keys
- `Ctrl+Z` / `Ctrl+Y` - undo / redo
- `Ctrl+K` - palette
- `Ctrl+0` - fit, `Ctrl+3` - 3D, `Ctrl+PageUp/PageDown` - levels
- `Esc` - step back

## tips
- The mnemonic table is tested for uniqueness and for conflicts with system shortcuts.
- Each command's mnemonic is shown in the palette and in the ribbon tooltip.
