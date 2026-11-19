# 2D detailing
@tags detail, hatch, filled region, revision cloud, legend, detail line
@context ann.detail ann.region ann.detail_line ann.cloud ann.legend
@commands ann.detail ann.region ann.detail_line ann.cloud ann.legend
@see tags_text dimensions sheets

2D elements exist in one view only and are not part of the model: details, lines, filled regions, revision clouds and legends.

## steps
1. Detail line: `DL` ([[cmd:ann.detail_line]]) - segments over the view.
2. Hatch and fill: `FG` ([[cmd:ann.region]]) - draw a closed outline and choose a pattern.
3. Library detail: `DC` ([[cmd:ann.detail]]) - insert a ready-made detail.
4. Revision cloud: `RC` ([[cmd:ann.cloud]]) - circle the changed area.
5. Legend: `LG` ([[cmd:ann.legend]]) - a key of symbols for a sheet.
6. Place the view on a sheet ([[topic:sheets]]) - 2D elements go to print.

## keys
- `DL` `FG` `DC` `RC` `LG`
- `Enter` - finish the outline
- `C` - close the line
- `Backspace` - remove the last point
- `Esc` - exit

## tips
- 2D elements belong to the view, not to the model: they are absent on other plans.
- Place clouds right before issuing sheets with changes.
- Lines and regions are removed by normal selection and `Del`.
