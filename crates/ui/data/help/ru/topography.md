# Рельеф
@tags рельеф, топография, поверхность, точки, площадка, горизонтали, terrain, topography, toposolid, site
@context site.toposolid site.edit_points
@commands site.toposolid site.toposolid_import site.boundary site.subregion site.pad site.smooth site.flatten site.edit_points site.contours site.properties
@see coordinates views_3d levels

Рельеф: поверхность участка, построенная по точкам с отметками. Команды: на вкладке «Архитектура», группа участка.

## steps
1. Рельеф по точкам: [[cmd:site.toposolid]]: щёлкайте точки на плане, число + `Enter` задаёт отметку следующих точек, `Enter`: создать.
2. Рельеф из файла съёмки: [[cmd:site.toposolid_import]]: точки CSV, TXT, XYZ, PTS, TSV, а также DXF или IFC.
3. Граница рельефа: [[cmd:site.boundary]]; подобласть со своим материалом: [[cmd:site.subregion]].
4. Площадка здания: [[cmd:site.pad]]: обведите контур, число + `Enter`: отметка; объёмы выемки и насыпи видны в строке состояния.
5. Сгладить или выровнять область: [[cmd:site.smooth]], [[cmd:site.flatten]]; точки по одной: [[cmd:site.edit_points]] (в 3D тяните ручку по вертикали).
6. Горизонтали показываются и скрываются командой [[cmd:site.contours]]; свойства площадки: [[cmd:site.properties]].

## keys
- `Enter`: завершить контур или применить
- `Backspace`: убрать последнюю точку
- `Esc`: выход

## tips
- Рельеф удобно готовить по подложке DWG или PDF ([[topic:import_cad]]).
- Положение проекта относительно общих координат: [[topic:coordinates]].
- Для крупных участков точек должно быть достаточно, чтобы поверхность не «провисала» между ними.
