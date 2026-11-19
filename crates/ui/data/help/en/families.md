# Families, types and the library
@tags family, type, library, .rfa, type manager
@context mgr.library mgr.load mgr.types mod.change_type
@commands mgr.library mgr.load mgr.types mgr.params mod.change_type
@see components parameters wall_layers

A family is an element template; a type is its set of sizes and parameters. The library stores families; only the needed ones are loaded into the project.

**Not available yet:** loading a family into the project (`mgr.load`). The ribbon buttons of these commands are inactive.

## steps
1. Open [[cmd:mgr.library]] - the "Component library": a search field and the families by category (furniture, doors, windows, plumbing, lighting, equipment...).
2. Pick a family there; `.rfa` families from files are read through the installed `.rvt` / `.rfa` module ([[topic:import_rvt]]).
3. Manage types in [[cmd:mgr.types]] (`TY`): create, duplicate, rename, edit parameters.
4. Type parameters are edited in the parameter window ([[cmd:mgr.params]], `PA`); all instances of the type update at once.
5. To change the type of the selection press `CY` ([[cmd:mod.change_type]]) - one operation, one `Ctrl+Z`.
6. Removing unused types ("Purge unused") is planned: [[topic:files_projects]].

## keys
- `TY` - type manager
- `PA` - family parameters
- `CY` - change type
- `Ctrl+Z` - undo

## tips
- The parameter editor writes type values only; formulas and defaults inside the family are not changed here.
- Wall and floor layers and rebar sections are types too: [[topic:wall_layers]].
- Parameters shared across the whole project: [[topic:parameters]].
