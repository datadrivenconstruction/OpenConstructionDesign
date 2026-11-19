# AI assistant
@tags AI, assistant, explain, ask, suggestions
@context ai.ask ai.panel ai.explain ai.fix
@commands ai.ask ai.panel ai.settings ai.what_sent ai.audit ai.explain ai.fix mod.explain
@see warnings checks_compare command_palette

The AI assistant performs tasks from a description, explains the selection and proposes fixes for check results. Every action is shown in advance and undone like normal.

## steps
1. Press `Ctrl+J` ([[cmd:ai.ask]]) and describe the task in your own words: "put a door in every room".
2. The first time, connect a model ([[cmd:ai.settings]]): local or cloud. The key stays in the system's secure store; for a cloud address the project asks for consent.
3. The assistant shows a plan: how many created, changed, deleted. Hover an item to see it on the plan and in 3D; uncheck what you do not want.
4. `Ctrl+Enter` applies what is checked, the Reject button discards the plan.
5. Select an element and run `AE` ([[cmd:ai.explain]]): the assistant explains what it is and where its sizes come from.
6. After checks run `AF` ([[cmd:ai.fix]]) - suggested fixes for each remark.
7. What is applied is one history entry: `Ctrl+Z` undoes it.
8. A precise geometric explanation without AI - `WY` ([[cmd:mod.explain]]).

## keys
- `Ctrl+J` - ask / do
- `AE` - explain selection, `AF` - fixes
- `Ctrl+Z` - undo

## tips
- Be specific: category, level, value.
- Always verify the result in 3D or plan.
- Suggestions are not applied without your confirmation.
- [[cmd:ai.what_sent]] shows the requests as they went out.
