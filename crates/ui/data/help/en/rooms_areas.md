# Rooms, separators, zones
@tags room, area, zone, separator, floor area
@context arch.room arch.wall_room arch.room_sep arch.zone sel:room
@commands arch.room arch.wall_room arch.room_sep arch.zone ann.tag_all
@see tags_text schedules ceilings

A room is defined by a closed loop of walls and separators and holds area, perimeter, volume and name. The area is recomputed on every wall change.

**Not available yet:** the room separator (`RS`) and the zone (`ZN`). The ribbon buttons of these commands are inactive.

## steps
1. Type `RM` ([[cmd:arch.room]]) or `WR` ([[cmd:arch.wall_room]]): both draw a room as a rectangle of four walls - click two opposite corners or type the sizes.
2. A separator (`RS`, [[cmd:arch.room_sep]]) for an open plan is planned.
3. Any closed wall loop can also be drawn with the wall tool: `WA`, `C` closes the outline ([[topic:walls]]).
4. A zone (`ZN`, [[cmd:arch.zone]]) that groups rooms for calculations is planned.
5. Tag all rooms with `TA` ([[cmd:ann.tag_all]]); name and number are set in the properties.
6. Areas are listed in the schedule table ([[topic:schedules]]).

## keys
- `RM` / `WR` - room as a rectangle of walls
- `TA` - tag all

## tips
- A room follows its walls: move a wall and the area updates.
- When importing IFC, rooms come from spaces; unclosed ones get separators.
