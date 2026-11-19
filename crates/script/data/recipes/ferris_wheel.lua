-- Ferris wheel: rim, spokes, A-frame legs and hangers as structural member rows, a gondola cabin on every rim node.
local n = param("gondolas", 24, {min = 12, max = 48})
local radius = param("radius", 20, {min = 8, max = 60})
local hubz = radius + 3
local node = {}
for k = 0, n - 1 do
  local a = k * 2 * math.pi / n
  node[k] = vec3(0, radius * math.sin(a), hubz + radius * math.cos(a))
end
local hub = vec3(0, 0, hubz)
local main, light = "CHS 168.3x8", "CHS 88.9x5"
for k = 0, n - 1 do
  member("rim", node[k], node[(k + 1) % n], main, "beam")
  member("spoke", hub, node[k], main, "brace")
end
member("axle", vec3(-3, 0, hubz), vec3(3, 0, hubz), main, "beam")
local base = radius * 0.4
for _, x in ipairs({-3, 3}) do
  member("leg", vec3(x, -base, 0), vec3(x, 0, hubz), main, "brace")
  member("leg", vec3(x, base, 0), vec3(x, 0, hubz), main, "brace")
end
local cabin = define_component("gondola", {
  extrude(profile.rect(1.6, 1.6), vec3(0, 0, -2.4), vec3(0, 0, -0.6)),
})
for k = 0, n - 1 do
  member("hanger", node[k], node[k] - vec3(0, 0, 0.6), light, "brace")
  place(cabin, node[k], "gondola")
end
audit{strict = true}
