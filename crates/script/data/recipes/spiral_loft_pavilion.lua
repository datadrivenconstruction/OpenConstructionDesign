-- Spiral loft pavilion: ellipses turning and swelling with height, lofted into a diagrid shell.
local height = param("height", "12 m", {min = "4 m", max = "40 m"})
local turn = param("turn", "60 deg", {min = "0 deg", max = "180 deg"})
local curves = {}
for k = 0, 4 do
  local t = k / 4
  local a, b, r = 9 + 2 * math.sin(math.pi * t), 6 + 1.5 * math.sin(math.pi * t), turn * t
  local c = {}
  for i = 0, 32 do
    local s = 2 * math.pi * i / 32
    local x, y = a * math.cos(s), b * math.sin(s)
    c[#c + 1] = vec3(x * math.cos(r) - y * math.sin(r), x * math.sin(r) + y * math.cos(r), height * t)
  end
  curves[#curves + 1] = c
end
surface_grid{surface = surf.loft{curves = curves}, nu = 24, nv = 6, closed_u = true,
             pattern = "diagrid", member = "CHS 139.7x8", name = "pavilion"}
audit{strict = true}
