# Проёмы и шахты
@tags проём, шахта, отверстие, shaft, opening, cut
@context arch.shaft arch.opening str.slab_opening
@commands arch.shaft arch.opening str.slab_opening arch.roof_opening arch.ceiling_opening arch.dormer
@see floors roofs ceilings doors_windows

Проём вырезается в одном элементе-хозяине; шахта: вертикальный проём, режущий все перекрытия, крыши и потолки на своём пути.

**Пока недоступно:** проём в стене (`OP`). Кнопки этих команд на ленте неактивны.

## steps
1. Проём в стене: `OP` ([[cmd:arch.opening]]) запланирован; пока вырезайте дверь или окно в стене ([[topic:doors_windows]]).
2. Проём в перекрытии: `SO` ([[cmd:str.slab_opening]]): нарисуйте контур прямо на плане.
3. Шахта: `SH` ([[cmd:arch.shaft]]): щёлкайте точки контура, `Enter` завершает.
4. Проёмы в крыше и потолке: [[cmd:arch.roof_opening]] и [[cmd:arch.ceiling_opening]]; слуховое окно: [[cmd:arch.dormer]].
5. Контур проёма правится ручками.
6. Результат виден на плане, разрезе и в 3D.

## keys
- `SO`: проём в перекрытии
- `SH`: шахта
- `Enter`: завершить контур, `Esc`: выход

## tips
- Шахта режет все пересекаемые перекрытия, крыши и потолки: менять каждое не нужно.
- Удалить проём: удалить его элемент, хозяин восстановится целым.
