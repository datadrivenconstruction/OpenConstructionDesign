# Boolean operations on forms
@tags union, subtract, intersect, boolean, join, cut
@context mass.union mass.subtract mass.intersect mass.join mass.unjoin mass.cut
@commands mass.union mass.subtract mass.intersect mass.join mass.unjoin mass.cut
@see masses mass_by_face

Boolean operations add, subtract and intersect solids. The result stays editable: source forms do not vanish.

## steps
1. Run the command and click the forms in 3D, one by one.
2. Union - [[cmd:mass.union]]; subtract - [[cmd:mass.subtract]] (first the form to cut from, then the one to subtract); intersect - [[cmd:mass.intersect]].
3. For a persistent geometry join of two elements: [[cmd:mass.join]]; to undo it - [[cmd:mass.unjoin]].
4. Cut one element with another without merging - [[cmd:mass.cut]] (first the cutting element, then the one to cut).
5. Check the result in 3D: faces after the operation get labels.
6. Every operation is undone with one `Ctrl+Z`.

## keys
- `Ctrl+Z` - undo
- `Esc` - leave the command

## tips
- Void forms (`mass.void`) are the same subtraction but reversible and parametric: [[topic:masses]].
- Boolean results are validated: an invalid solid is not created and the reason is shown.
- Elements built on faces carry over to the new faces after the operation.
