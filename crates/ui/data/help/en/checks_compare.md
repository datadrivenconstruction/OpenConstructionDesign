# Checks, classification, compare
@tags check, IDS, rules, classification, compare, diff
@context data.check data.classify data.compare view.diff
@commands data.check data.classify data.compare view.diff mod.explain
@see warnings clash linked_models

Checks apply rules (including IDS) to the model; classifiers assign codes to elements; comparison shows differences between versions.

**Status: planned.** Checks and version comparison are not available in this build yet: their ribbon buttons are inactive. Classification and "Explain element" work.

## steps
1. `CK` ([[cmd:data.check]]) - pick a rule set and run. Violations are listed; clicking selects the element.
2. Fix and rerun - the green mark in the status bar shows the outcome.
3. Classification: `CF` ([[cmd:data.classify]]) - type a system and a code and assign it to the selected elements or to every element of their categories; the window lists the codes already there. A system is the text parameter `Classification.<system>`, so codes show in Properties and in tables.
4. Version comparison: `DF` ([[cmd:data.compare]]) - a list of differences; [[cmd:view.diff]] shows them in 3D.
5. Ask "why is it so": `WY` ([[cmd:mod.explain]]) lists in plain sentences what holds the selected element: its level, what its top and base are attached to, joins, rules, openings, pin, group, phase and warnings.
6. Results are useful for issuing: [[topic:export]].

## keys
- `CK` check, `CF` classify, `DF` compare, `WY` explain
- `Ctrl+Alt+I` - checks panel

## tips
- Rule sets are stored in a file and can be applied to different projects.
- For bulk fixes of the findings use [[topic:query_bulk]].
- Model warnings are a separate panel: [[topic:warnings]].
