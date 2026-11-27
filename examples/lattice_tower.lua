-- Lattice tower: four tapering legs, a ring at every panel and one diagonal per face, in three stages; members only.
local height = param("height", "60 m", {min = "20 m", max = "300 m"})
local base = param("base", "14 m", {min = "4 m", max = "80 m"})
local top = param("top", "3 m", {min = "1 m", max = "20 m"})
local panels = param("panels", 12, {min = 3, max = 60})

-- legs, rings and diagonals get lighter stage by stage
local stages = {
  {leg = "CHS 508x16", ring = "CHS 273x10", diag = "CHS 219.1x8"},
  {leg = "CHS 406.4x12.5", ring = "CHS 219.1x8", diag = "CHS 168.3x6.3"},
  {leg = "CHS 273x10", ring = "CHS 168.3x6.3", diag = "CHS 139.7x5"},
}
local signs = {{-1, -1}, {1, -1}, {1, 1}, {-1, 1}}

local function corner(i, z)
  local h = (base + (top - base) * z / height) / 2
  return vec3(signs[i][1] * h, signs[i][2] * h, z)
end

for s = 1, 3 do
  local p = stages[s]
  group("stage" .. s, function()
    local k0 = math.floor((s - 1) * panels / 3)
    local k1 = math.floor(s * panels / 3) - 1
    for k = k0, k1 do
      local z0, z1 = height * k / panels, height * (k + 1) / panels
      for i = 1, 4 do
        local j = i % 4 + 1
        member{from = corner(i, z0), to = corner(i, z1), profile = p.leg, role = "column"}
        if k == 0 then
          member{from = corner(i, z0), to = corner(j, z0), profile = p.ring, role = "brace"}
        end
        member{from = corner(i, z1), to = corner(j, z1), profile = p.ring, role = "brace"}
        if (k + i) % 2 == 0 then
          member{from = corner(i, z0), to = corner(j, z1), profile = p.diag, role = "diagonal"}
        else
          member{from = corner(j, z0), to = corner(i, z1), profile = p.diag, role = "diagonal"}
        end
      end
    end
  end)
end
audit{strict = true}
