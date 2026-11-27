-- Small house frame: a ground slab, steel columns and beams on a grid, and a flat roof slab.
local bays_x = param("bays_x", 3, {min = 1, max = 12})
local bays_y = param("bays_y", 2, {min = 1, max = 12})
local span = param("span", "4 m", {min = "2 m", max = "9 m"})
local storey = param("storey", "3 m", {min = "2.4 m", max = "6 m"})

local w, d = bays_x * span, bays_y * span
local beam_d = 0.3 -- depth of the IPE 300 beams
local floor_t, roof_t = 0.25, 0.2

slab{outline = {{-0.3, -0.3}, {w + 0.3, -0.3}, {w + 0.3, d + 0.3}, {-0.3, d + 0.3}},
     z = 0, thickness = floor_t, name = "ground"}

-- columns and beams meet on their axes at the nodes of the grid
local top = storey - beam_d / 2
for i = 0, bays_x do
  for j = 0, bays_y do
    local x, y = i * span, j * span
    member{from = vec3(x, y, 0), to = vec3(x, y, top), profile = "HEA 200", role = "column"}
  end
end
for j = 0, bays_y do
  member{from = vec3(0, j * span, top), to = vec3(w, j * span, top), profile = "IPE 300", role = "beam"}
end
for i = 0, bays_x do
  for j = 0, bays_y - 1 do
    member{from = vec3(i * span, j * span, top), to = vec3(i * span, (j + 1) * span, top),
           profile = "IPE 300", role = "beam"}
  end
end

slab{outline = {{-0.3, -0.3}, {w + 0.3, -0.3}, {w + 0.3, d + 0.3}, {-0.3, d + 0.3}},
     z = storey + roof_t, thickness = roof_t, name = "roof", role = "slab"}
audit{strict = true}
