# Query and bulk edit
@tags query, search, filter, bulk edit
@context data.query mod.select_filter mod.select_similar
@commands data.query mod.select_filter mod.select_similar mod.select_same_cat_project
@see selection parameters xlsx_sync

A query finds elements by category and conditions and can change the result in one operation. The query field is in the `Ctrl+K` palette; the same engine is available as [[cmd:data.query]] (`QY`).

The [[cmd:data.query]] window (`QY`) reads the query as you type: the count and a summary by category, or the error in plain words. Its main button selects the result or, with `->`, changes it in one operation; a valid query can be saved as a project filter, and a click on a saved filter selects what it finds.

## steps
1. Press `Ctrl+K` and type a query: `doors width>900` or `walls type=Masonry 200 level=2`.
2. Combine conditions with `and`, `or`, `not` and parentheses: `windows or doors`.
3. The result shows the number of matches; pick a row to select them in the view.
4. For a bulk edit add `->`: `doors width>900 -> width=width+100`.
5. An applied edit is one operation: `Ctrl+Z` reverts it whole.
6. Save frequent queries as project filters; "select similar" is `Ctrl+Shift+A` ([[cmd:mod.select_similar]]).

## keys
- `Ctrl+K` - palette with query
- `Enter` - run, `Esc` - close

## tips
- Operators: `=`, `<>`, `<`, `>`, `<=`, `>=`, `~` (contains). A bare word matches type, name and tag.
- Keywords also work in other supported UI languages, including Russian.
- Lengths without a unit are in project units; you can write `1.2m`.
