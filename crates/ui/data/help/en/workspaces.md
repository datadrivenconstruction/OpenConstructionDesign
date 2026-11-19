# Interfaces and panel layouts
@tags interface, layout, workspace, panels
@context ui.layout.settings ui.layout.next ui.layout.prev ui.layout.set.1 ui.layout.set.2 ui.layout.set.3 ui.layout.set.4 ui.layout.set.5 view.ui
@commands ui.layout.settings ui.layout.next ui.layout.prev ui.layout.set.1 ui.layout.set.2 ui.layout.set.3 ui.layout.set.4 ui.layout.set.5
@see interface shortcuts

There are five ready-made interfaces: "Classic ribbon", "Contextual", "Plan + 3D", "Focus" and "Model + table". Switch between them with one key combination to suit the task.

Panels fold to rails with `Ctrl+B` (project browser), `Ctrl+Alt+B` (properties) and `Ctrl+Alt+I` (checks); `Ctrl+Shift+F1` hides everything but the model view. In a narrow window (under 1280 px) the properties fold to a rail on their own and come back when there is room.

**Not available yet:** view tiling (`Ctrl+Alt+T`).

## steps
1. Press `Ctrl+Alt+1`...`Ctrl+Alt+5` to pick an interface ([[cmd:ui.layout.set.1]] - [[cmd:ui.layout.set.5]]).
2. Cycle: `Ctrl+Alt+L` ([[cmd:ui.layout.next]]) forward, `Ctrl+Alt+Shift+L` ([[cmd:ui.layout.prev]]) back.
3. "Plan + 3D" suits drawing with volume control; "Model + table" suits data; "Focus" strips everything extra.
4. Interface settings - [[cmd:ui.layout.settings]]: density, panels, ribbon.
5. Several views can be open in tabs; selection is shared between them.

## keys
- `Ctrl+Alt+1`...`5` - interfaces
- `Ctrl+Alt+L` / `Ctrl+Alt+Shift+L` - next / previous

## tips
- Selection is synchronised across tiled views: keep a plan, 3D and a table side by side.
- The chosen interface is remembered between launches.
