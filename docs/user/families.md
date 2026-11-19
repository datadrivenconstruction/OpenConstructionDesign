# Parametric families

A family is a component whose geometry depends on parameters such as width, height or
material. Families live inside the model; there is no separate editor.

- Place a component and choose a type, or create a new type by changing parameters.
- Changing a type parameter updates every instance of that type.
- Instance parameters apply to one placed element.
- Parameters appear in schedules and can be edited in bulk.
- Sandboxed Lua scripts can generate custom geometry from parameters.
- IFC export writes types and instances with their property sets.

See the scripting section of [ARCHITECTURE.md](../ARCHITECTURE.md) for the script interface.
