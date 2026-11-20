# Оборудование и приборы
@tags оборудование, светильник, розетка, сантехприбор, воздухораспределитель, equipment, fixture, light, terminal
@context mep.equipment mep.terminal mep.fixture mep.device mep.light
@commands mep.equipment mep.terminal mep.fixture mep.device mep.light mep.connect
@see mep mep_systems components

Приборы размещаются как компоненты, но имеют соединители, к которым подключаются трассы.

**Пока недоступно:** воздухораспределители (`AT`), сантехприборы (`PF`), электроприборы (`ED`), светильники (`LF`) и соединение трассы (`CN`). Кнопки этих команд на ленте неактивны.

## steps
1. Оборудование: `ME` ([[cmd:mep.equipment]]): ставится как компонент ([[topic:components]]).
2. Воздухораспределители, сантехприборы, электроприборы и светильники ([[cmd:mep.terminal]], [[cmd:mep.fixture]], [[cmd:mep.device]], [[cmd:mep.light]]) запланированы.
3. Выберите тип в панели опций, щёлкните положение; `Space` поворачивает на 90°.
4. Соединение трассы с коннектором прибора (`CN`, [[cmd:mep.connect]]) запланировано.
5. Объединяйте всё в систему: [[topic:mep_systems]].

## keys
- `ME`: оборудование
- `Space`: повернуть на 90°

## tips
- Новые семейства загружаются через библиотеку ([[topic:families]]).
