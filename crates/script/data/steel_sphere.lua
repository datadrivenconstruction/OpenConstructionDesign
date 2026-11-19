-- Steel sphere: latitude rings and meridians of straight tubes between nodes, a central
-- column, platforms at five rings and a spiral stair between the lowest and the highest.
-- Every member is straight and ends on a node (no bent rolled sections).
local R = param("radius", "12 m", {min = "4 m", max = "60 m"})
local rings = param("rings", 23, {min = 3, max = 200})
local meridians = param("meridians", 76, {min = 8, max = 400})
local base = param("base", "0.5 m")

local ring_p = profile.chs(len("168.3 mm"), len("8 mm"))
local mer_p = profile.shs(len("150 mm"), len("8 mm"))
local col_p = profile.chs(len("323.9 mm"), len("10 mm"))
local beam_p = profile.named("IPE 200")
local post_p = profile.chs(len("42.4 mm"), len("3.2 mm"))
local tread_p = profile.rect(len("300 mm"), len("40 mm"))
local stringer_p = profile.rect(len("12 mm"), len("200 mm"))

local C = vec3(0, 0, base + R)
local south, north = C - vec3(0, 0, R), C + vec3(0, 0, R)
local step = math.pi / (rings + 1)

local function phi(i) return -math.pi / 2 + i * step end
local function node(i, j)
  local f, t = phi(i), 2 * math.pi * (j % meridians) / meridians
  return C + vec3(math.cos(f) * math.cos(t), math.cos(f) * math.sin(t), math.sin(f)) * R
end

-- latitude rings: one straight tube per bay
for i = 1, rings do
  group("ring " .. i, function()
    for j = 0, meridians - 1 do
      part("tube", extrude(ring_p, node(i, j), node(i, j + 1)), "S355")
    end
  end)
end

-- meridians: pole to pole through every ring node
for j = 0, meridians - 1 do
  group("meridian " .. j + 1, function()
    part("bar", extrude(mer_p, south, node(1, j)), "S355")
    for i = 1, rings - 1 do
      part("bar", extrude(mer_p, node(i, j), node(i + 1, j)), "S355")
    end
    part("bar", extrude(mer_p, node(rings, j), north), "S355")
  end)
end

-- central column in storeys of about one metre
local storeys = math.max(1, math.floor(2 * R))
group("column", function()
  for k = 0, storeys - 1 do
    local a = south + vec3(0, 0, 2 * R * k / storeys)
    local b = south + vec3(0, 0, 2 * R * (k + 1) / storeys)
    part("segment", extrude(col_p, a, b), "S355")
  end
end)

-- platforms at five rings: radial beams to every fourth node, deck plates, railing
local levels = {}
for k = 1, 5 do levels[k] = math.floor(k * (rings + 1) / 6) end
local every = 4
for _, i in ipairs(levels) do
  local z = node(i, 0).z
  local axis = vec3(0, 0, z)
  group("platform " .. i, function()
    for j = 0, meridians - 1, every do
      local beam = part("beam", extrude(beam_p, axis, node(i, j)), "S355")
      constrain("coincident", beam.b, node(i, j))
      local a, b = node(i, j), node(i, j + every)
      local inset = 0.96
      local pa, pb = (a - axis) * inset, (b - axis) * inset
      local plate = profile.polygon({{0, 0}, {pa.x, pa.y}, {pb.x, pb.y}})
      part("deck", extrude(plate, axis + vec3(0, 0, 0.1), axis + vec3(0, 0, 0.13)), "S235")
      local top = vec3(0, 0, 1.1)
      local pa3, pb3 = axis + pa * 0.97, axis + pb * 0.97
      part("post", extrude(post_p, pa3 + vec3(0, 0, 0.13), pa3 + top), "S235")
      part("rail", extrude(post_p, pa3 + top, pb3 + top), "S235")
    end
  end)
end

-- spiral stair from the lowest platform to the highest
local z0 = node(levels[1], 0).z + 0.13
local z1 = node(levels[#levels], 0).z + 0.13
local rise, turn = 0.18, deg(22.5)
local steps = math.floor((z1 - z0) / rise)
local r_in, r_out = 0.17, 1.2
local function at(r, k) return vec3(r * math.cos(k * turn), r * math.sin(k * turn), z0 + k * rise) end
group("stair", function()
  for k = 1, steps do
    part("tread", extrude(tread_p, at(r_in, k), at(r_out, k)), "S235")
    part("stringer", extrude(stringer_p, at(r_out, k - 1) - vec3(0, 0, 0.1),
      at(r_out, k) - vec3(0, 0, 0.1)), "S235")
    part("baluster", extrude(post_p, at(r_out - 0.03, k), at(r_out - 0.03, k) + vec3(0, 0, 1.0)), "S235")
    part("handrail", extrude(post_p, at(r_out - 0.03, k - 1) + vec3(0, 0, 1.0),
      at(r_out - 0.03, k) + vec3(0, 0, 1.0)), "S235")
  end
end)

local d = measure(south, north)
assert(math.abs(d - 2 * R) < 1e-6, "pole to pole must equal the diameter")
print("parts", rings * meridians + meridians * (rings + 1) + storeys + 4 * steps)
