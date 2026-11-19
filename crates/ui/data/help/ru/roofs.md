# Крыши
@tags крыша, скат, фронтон, слуховое окно, roof, gable, dormer, extrusion
@context arch.roof arch.roof_extrusion arch.dormer arch.roof_opening
@commands arch.roof arch.roof_extrusion arch.dormer arch.roof_opening mass.roof_by_face
@see wall_profile openings_shafts ceilings

Крышу строят по контуру (скаты с заданным уклоном и фронтонами) или выдавливанием профиля вдоль оси. Слуховые окна и проёмы в крыше режут крышу по эскизу.

## steps
1. Крыша по контуру: `RF` ([[cmd:arch.roof]]) — щёлкайте точки контура на плане; `Enter` или щелчок в первую точку завершает.
2. У каждого ребра есть флажок «задаёт уклон»: `Space` переключает его у последнего ребра (фронтонное ребро уклона не имеет). Угол уклона — в панели опций.
3. Крыша выдавливанием: выберите одну прямую стену и запустите `RX` ([[cmd:arch.roof_extrusion]]) — над стеной строится двускатный профиль 30° на глубину до дальней параллельной стены.
4. Слуховое окно: [[cmd:arch.dormer]] — контур проёма на скате.
5. Проём в крыше: [[cmd:arch.roof_opening]] — контур по точкам.
6. Привяжите верх стен к крыше командой «Присоединить верх» ([[topic:wall_profile]]).
7. Форма выбранной крыши — [[cmd:arch.roof_gable]], [[cmd:arch.roof_hip]], [[cmd:arch.roof_mansard]]; присоединение — [[cmd:arch.roof_join]]; кромки — лобовая доска [[cmd:arch.fascia]], водосток [[cmd:arch.gutter]], зенитный фонарь [[cmd:arch.skylight]].

## keys
- `RF` — крыша по контуру, `RX` — выдавливанием
- `Enter` — завершить контур
- `Space` — флажок «задаёт уклон» у последнего ребра
- `Esc` — выход

## tips
- Крыша по грани формы — [[cmd:mass.roof_by_face]] ([[topic:mass_by_face]]).
- Угол уклона задаётся в панели опций до завершения крыши.
