# Интерфейсы и раскладки панелей
@tags интерфейс, раскладка, рабочее пространство, панели, layout, workspace, panels
@context ui.layout.settings ui.layout.next ui.layout.prev ui.layout.set.1 ui.layout.set.2 ui.layout.set.3 ui.layout.set.4 ui.layout.set.5 view.ui
@commands ui.layout.settings ui.layout.next ui.layout.prev ui.layout.set.1 ui.layout.set.2 ui.layout.set.3 ui.layout.set.4 ui.layout.set.5
@see interface shortcuts

Есть пять готовых интерфейсов: «Классическая лента», «Контекстный», «План + 3D», «Фокус» и «Модель + таблица». Переключайтесь между ними одним сочетанием клавиш под задачу.

Панели сворачиваются в узкие полосы: `Ctrl+B` — диспетчер проекта, `Ctrl+Alt+B` — свойства, `Ctrl+Alt+I` — проверки; `Ctrl+Shift+F1` скрывает всё, кроме вида модели. В узком окне (меньше 1280 px) свойства сворачиваются сами и возвращаются, когда места снова хватает.

**Пока недоступно:** сетка видов (`Ctrl+Alt+T`).

## steps
1. Нажмите `Ctrl+Alt+1`...`Ctrl+Alt+5`, чтобы выбрать интерфейс ([[cmd:ui.layout.set.1]] — [[cmd:ui.layout.set.5]]).
2. Листайте по кругу: `Ctrl+Alt+L` ([[cmd:ui.layout.next]]) вперёд, `Ctrl+Alt+Shift+L` ([[cmd:ui.layout.prev]]) назад.
3. «План + 3D» удобен для построения с контролем объёма; «Модель + таблица» — для данных; «Фокус» убирает всё лишнее.
4. Настройки интерфейса — [[cmd:ui.layout.settings]]: плотность, панели, лента.
5. Несколько видов можно держать во вкладках; выбор у них общий.

## keys
- `Ctrl+Alt+1`...`5` — интерфейсы
- `Ctrl+Alt+L` / `Ctrl+Alt+Shift+L` — следующий / предыдущий

## tips
- Выбор синхронизирован между видами сетки: удобно держать план, 3D и таблицу рядом.
- Выбранный интерфейс запоминается между запусками.
