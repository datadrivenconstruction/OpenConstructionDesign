-- Ruled shell hall: straight rulings from a level eave to a waving ridge, columns along both edges.
local length = param("length", "40 m", {min = "16 m", max = "120 m"})
local depth = param("depth", "20 m", {min = "10 m", max = "40 m"})
local a, b = {}, {}
for i = 0, 10 do
  local x = length * i / 10
  a[#a + 1] = vec3(x, 0, 6)
  b[#b + 1] = vec3(x, depth, 10 + 2 * math.sin(math.pi * 2 * i / 10))
end
surface_grid{surface = surf.ruled{a = a, b = b}, nu = 20, nv = 8, pattern = "quad",
             panels = "60 mm", panel_material = "timber CLT", edge_columns = 2,
             name = "hall"}
audit{strict = true}
