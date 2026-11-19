# Уровни
@tags уровень, этаж, отметка, высота, level, storey, elevation
@context mgr.level arch.level str.level mgr.level_rename mgr.level_delete view.level_up view.level_down view.underlay
@commands mgr.level arch.level mgr.level_rename mgr.level_delete view.level_up view.level_down view.underlay view.plan
@see grids walls navigation_2d

Уровни задают отметки по высоте: к ним привязаны стены, перекрытия, планы уровней. Вид плана всегда относится к уровню; на нём рисуется всё новое.

## steps
1. Уровни и их отметки видны в диспетчере проекта и на разрезе / фасаде ([[topic:sections_elevations]]).
2. Запустите [[cmd:mgr.level]] (`LV`): диалог «Новый уровень» запрашивает имя и отметку в метрах (например `3,000`); уровень создаётся и становится активным.
3. План всегда относится к активному уровню; [[cmd:view.plan]] возвращает на план.
4. [[cmd:mgr.level_rename]] (`LR`) открывает диалог «Уровень» для текущего уровня (имя и отметка); [[cmd:mgr.level_delete]] (`LX`) удаляет текущий уровень.
5. Переключайтесь между уровнями `Ctrl+PageUp` / `Ctrl+PageDown`.
6. Для совмещения стен включите подложку нижнего уровня — `UL` ([[cmd:view.underlay]]).

## keys
- `LV` — новый уровень
- `LR` — переименовать / изменить отметку, `LX` — удалить
- `Ctrl+PageUp` / `Ctrl+PageDown` — уровень выше / ниже
- `UL` — подложка нижнего уровня

## tips
- Стены, привязанные к уровню, следуют за его отметкой; верх стены можно привязать к вышележащему уровню ([[topic:wall_profile]]).
- Марки и границы уровней свои на каждом виде; на разрезе их можно тянуть ручками.
