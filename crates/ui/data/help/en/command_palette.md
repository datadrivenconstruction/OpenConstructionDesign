# Command palette and search
@tags palette, search, find, command, where is
@context app.palette app.where_is app.find
@commands app.palette app.where_is app.find app.repeat
@see shortcuts query_bulk interface

The `Ctrl+K` palette finds everything: commands, views, levels, elements, parameters and queries. It shows each command's ribbon path and key.

**Not available yet:** repeat last command. Its ribbon button is inactive.

## steps
1. Press `Ctrl+K` ([[cmd:app.palette]]) or click the search field in the top bar and start typing a name: any word is enough.
2. Pick a result with the arrows, `Enter` runs it. The command runs with the current selection.
3. A query like `doors width>900` lists elements, and `->` changes them ([[topic:query_bulk]]).
4. `Ctrl+Shift+K` ([[cmd:app.where_is]]) - "Where is...": describe a familiar concept in your own words and the editor shows our command.
5. `Ctrl+F` ([[cmd:app.find]]) opens the find window on the find field; `Ctrl+H` ([[cmd:edit.find_replace]]) adds the replace row. "Select all matches" selects the found elements. To find elements, views and parameters by name use this palette.
6. Repeat the last command - [[cmd:app.repeat]].

## keys
- `Ctrl+K` - palette
- `Ctrl+Shift+K` - "Where is..."
- `Ctrl+F` - find text
- `Ctrl+H` - find and replace text
- `Ctrl+P` - go to view

## tips
- The palette also lists elements, views, levels and saved project filters.
- Recently used commands come first.
