# Экспорт: IFC, glTF, STL, OBJ, PDF, xlsx
@tags экспорт, IFC, glTF, PDF, xlsx, export, exchange
@context data.export_ifc data.export_gltf
@commands data.export_ifc data.export_gltf data.export_pdf data.export_xlsx
@see printing xlsx_sync import_ifc

Модель выгружается в открытые форматы: IFC — для обмена между BIM-программами, glTF — для просмотра и визуализации, PDF/SVG — листы, `.xlsx` — таблицы.

**Пока недоступно:** экспорт IFC (`IF`) и сочетание `XE`. Кнопки этих команд на ленте неактивны.

## steps
1. IFC: `IF` ([[cmd:data.export_ifc]]) — выберите версию и состав (весь проект или выбранное).
2. glTF: `XG` ([[cmd:data.export_gltf]]) — вся модель, все уровни, с id и свойствами элементов в extras узлов. Скрытое или отсечённое в 3D-виде тоже попадает в файл.
   STL: `XS` ([[cmd:data.export_stl]]) и OBJ: `XO` ([[cmd:data.export_obj]]) — вся модель или выделение, в метрах или миллиметрах. OBJ пишет рядом файл `.mtl`, группа на элемент с его id в имени.
3. Листы в PDF/SVG: `XP` ([[cmd:data.export_pdf]]) — [[topic:printing]].
4. Таблицы: меню «Файл > Экспорт в XLSX…» и меню «Экспорт .xlsx» в спецификации — [[topic:xlsx_sync]].
5. Укажите имя файла в диалоге сохранения; результат показан в строке состояния.
6. Для проверки откройте результат в стороннем просмотрщике или импортируйте обратно ([[topic:import_ifc]]).

## keys
- `XP` — PDF/SVG

## tips
- Перед передачей запустите проверку предупреждений: [[topic:warnings]].
- Стены, проёмы, перекрытия, помещения и навесные стены выгружаются в IFC родными классами.
