# Parameters
@tags parameters, properties, project parameters, fields
@context mgr.params mgr.project_params
@commands mgr.params mgr.project_params mgr.types
@see families query_bulk schedules

Parameters hold element data: sizes, materials, marks. Family parameters belong to the type; project parameters add shared fields to many categories at once.

## steps
1. Select an element - the properties panel shows its parameters split into instance and type.
2. Type parameters are edited in the [[cmd:mgr.params]] window (`PA`): the change applies to every element of the type.
3. A shared parameter for categories (e.g. "Fire rating"): [[cmd:mgr.project_params]] (`PJ`) - name, data type, categories.
4. A new parameter is immediately available in schedules, filters and queries.
5. Fill values in the properties, in a schedule or in bulk through `.xlsx` ([[topic:xlsx_sync]]).
6. For conditions and bulk edits use [[topic:query_bulk]].

## keys
- `PA` - family parameters, `PJ` - project parameters
- `Ctrl+Z` - undo

## tips
- Choose the data type (number, length, text, yes/no) up front: changing it later is harder.
- A type edit is a single undo operation.
