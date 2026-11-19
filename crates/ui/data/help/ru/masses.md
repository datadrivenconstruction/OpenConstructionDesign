# Формы (массы)
@tags форма, масса, объём, выдавливание, тяни-толкай, mass, extrude, revolve, sweep, push pull
@context mass.extrude mass.revolve mass.sweep mass.blend mass.swept_blend mass.void mass.push_pull mass.fillet mass.chamfer mass.shell mass.offset mass.split_face mass.push_region mass.draft mass.paint sel:mass
@commands mass.extrude mass.revolve mass.sweep mass.blend mass.swept_blend mass.void mass.push_pull mass.fillet mass.chamfer mass.shell mass.offset mass.split_face mass.push_region mass.draft mass.paint
@see mass_booleans mass_by_face views_3d

Вкладка «Формы» — для эскизного проектирования объёмов. Форма — тело с устойчивыми гранями; по её граням потом строят стены, перекрытия, крыши и витражи.

## steps
1. Запустите [[cmd:mass.extrude]] и щёлкайте точки контура на плане; `Enter` замыкает контур, затем введите высоту. Другие способы: [[cmd:mass.revolve]] (центр, радиус, высота), [[cmd:mass.sweep]] (путь, размер сечения), [[cmd:mass.blend]], [[cmd:mass.swept_blend]].
2. Пустотная форма [[cmd:mass.void]] вырезает объём из пересекающихся тел.
3. В 3D укажите грань; [[cmd:mass.push_pull]] двигает её: тяните с живым превью или введите расстояние и нажмите `Enter`.
4. Укажите две смежные грани и введите радиус для [[cmd:mass.fillet]] (скругление) или катет для [[cmd:mass.chamfer]] (фаска); выдолбить форму — [[cmd:mass.shell]] (открытые грани или `Enter` для замкнутой оболочки, введите толщину); сдвиг граней — [[cmd:mass.offset]].
5. Разделите грань посередине ([[cmd:mass.split_face]]) и покрасьте её следующим материалом ([[cmd:mass.paint]]).
   Контур прямо на плоской грани — [[cmd:mass.push_region]] (грань — рабочая плоскость; щёлкните первый угол или дважды последний, введите высоту); наклон граней — [[cmd:mass.draft]] (введите угол; горизонтальная грань задаёт нейтральную плоскость).
   Свободная поверхность: [[cmd:mass.freeform]] — гладкая поверхность по двум углам в плане (введите подъём); выберите её в 3D и введите размер панели для [[cmd:mass.panelize]], толщину кровли для [[cmd:mass.surface_shell]] или толщину оболочки для [[cmd:mass.thicken]].
6. Дальше: логические операции ([[topic:mass_booleans]]) и элементы по граням ([[topic:mass_by_face]]).

## keys
- `Push/Pull` в 3D: тяните грань или введите расстояние
- `Enter` — применить, `Esc` — отмена
- `Ctrl+Z` — отмена, `Ctrl+3` — 3D

## tips
- Грани имеют устойчивые идентификаторы: элементы, построенные по грани, следуют за ней при правке формы.
- Если грань исчезла, элемент остаётся «осиротевшим» с предупреждением, а не пропадает.
- Для оценки объёма и площади используйте спецификации ([[topic:schedules]]).
