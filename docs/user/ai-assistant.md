# AI assistant

The assistant turns a request in plain words into model edits. It never edits your model
directly.

## How it works

1. You describe a change ("add a stair between level 1 and level 2").
2. The assistant plans on a scratch copy of the model using the same editing tools you have.
3. You see a plan card, a preview and a diff of what would change.
4. **Apply** merges the change into your model as a single undo step. **Discard** drops it.

## Choose your provider

The assistant is not tied to one vendor. In the AI settings you enter the endpoint, model name
and key of any provider that offers a compatible HTTP chat interface, or the address of a
server running on your own machine. You pay your provider directly; the project runs no AI
service of its own.

- On Windows the key is kept in the operating system key store; elsewhere it is kept in
  memory only.
- A network egress policy lists the hosts the editor may call. Without a configured provider
  the editor makes no AI requests.
- What is sent: your request and the model context needed for it (element lists,
  parameters). Use a local server if the model must not leave your machine.

## Tips

- Ask for one change at a time and read the diff before Apply.
- Results depend on the model you choose; larger models handle multi-step requests better.
- An MCP server lets external clients use the same tools, with the same preview and Apply gate.
