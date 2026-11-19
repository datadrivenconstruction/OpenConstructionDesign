-- Membrane canopy: a four-point saddle sail, form-found, edge cables, masts and guys.
local size = param("size", "16 m", {min = "6 m", max = "40 m"})
local high = param("high", "7 m", {min = "4 m", max = "15 m"})
membrane{corners = {vec3(0, 0, 3), vec3(size, 0, high), vec3(size, size, 3), vec3(0, size, high)},
         rings = 8, divisions = 6, name = "canopy"}
audit{strict = true}
