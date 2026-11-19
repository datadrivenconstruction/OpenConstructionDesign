# Tags, text, leaders, numbering
@tags tag, text, leader, numbering, annotation
@context ann.tag ann.tag_all ann.text ann.number ann.leader
@commands ann.tag ann.tag_all ann.text ann.number ann.leader
@see dimensions rooms_areas schedules

Annotations label elements in a view: a tag reads element parameters (type, number, area), text is typed by hand, a leader points at an element.

## steps
1. Tag one element: `TG` ([[cmd:ann.tag]]) - click a door, window, wall or room; a tag can be dragged by its head, `Delete` removes it.
2. Tag all: `TA` ([[cmd:ann.tag_all]]) - pick the categories; the dialog shows how many tags will be added.
3. Numbering: `NU` ([[cmd:ann.number]]) - each click puts the next number (+1) on the element; one click is one undo step.
4. Free text (`TX`, [[cmd:ann.text]]) and leaders (`LD`, [[cmd:ann.leader]]): click the position, type several lines right in the view; `Ctrl+Enter` or a click outside the box accepts. A second click on the selected text reopens editing.
5. Tag text comes from parameters - change them in the properties and the tag updates.

## keys
- `TG` tag, `TA` tag all, `NU` numbering
- `Delete` - remove the tag under the cursor, `Esc` - exit

## tips
- A tag with an empty number shows "?".
- Tags belong to a view: on another plan they have to be placed separately.
