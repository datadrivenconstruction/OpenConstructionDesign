# Правка: перенос, копия, поворот и др.
@tags изменить, перенести, копировать, повернуть, зеркало, массив, выровнять, move, copy, rotate, mirror, array
@context mod.move mod.copy mod.rotate mod.scale mod.mirror mod.mirror_draw mod.array mod.offset mod.trim mod.split mod.align mod.extend mod.match mod.delete mod.cut_geom
@commands mod.move mod.copy mod.rotate mod.scale mod.mirror mod.mirror_draw mod.array mod.offset mod.trim mod.split mod.align mod.extend mod.match mod.delete
@see selection snaps undo_history clipboard

Команды правки работают с текущим выбором: сначала выберите элементы, затем вызовите команду. Стыки, проёмы и размеры пересчитываются автоматически, а результат: одна запись в истории.

**Пока недоступно:** перенос свойств (`MA`). Кнопки этих команд на ленте неактивны.

## steps
1. Запустите команду; она сама запросит выбор («выберите элементы, `Enter`: готово») либо возьмёт заранее выбранные элементы ([[topic:selection]]).
2. Команды: [[cmd:mod.move]] (`MV`), [[cmd:mod.copy]] (`CO`), [[cmd:mod.rotate]] (`RO`), [[cmd:mod.mirror]] (`MM`), [[cmd:mod.array]] (`AR`).
3. Укажите базовую и вторую точки, либо введите число: расстояние (перенос), угол (поворот), число копий (массив). В панели опций есть «Копия», «Ограничить», «Несколько», «Число», «До последней», «Радиально».
4. Стены: [[cmd:mod.align]] (`AL`, опорная линия, затем стена), [[cmd:mod.offset]] (`OF`, щелчок со стороны смещения и расстояние), [[cmd:mod.trim]] (`TR`, подрезка в угол), [[cmd:mod.extend]] (`EX`, граница, затем стены), [[cmd:mod.split]] (`SL`, щелчок на стене или расстояние от конца).
5. Подтвердите `Enter` или щелчком. Зеркало по нарисованной оси: [[cmd:mod.mirror_draw]] (`DM`); оригинал сохраняется опцией «Копия».
6. Удаление выбранного: [[cmd:mod.delete]] (`DE`, `Del`).

## keys
- `MV` `CO` `RO` `MM` `DM`: перенос, копия, поворот, зеркало (по оси / рисуя ось)
- `AR`: массив, `OF`: смещение, `AL`: выровнять
- `TR` / `EX` / `SL`: обрезать, удлинить, разделить
- `Space`: при повороте: выбрать новый центр
- `Del`: удалить

## tips
- Выбранные элементы можно двигать и за ручки, без команды.
- Привязки (`Tab`) работают во всех командах изменения: [[topic:snaps]].
