-- Wind turbine: tapered tower, nacelle, hub and three blades, each blade and the hub a component.
local height = param("height", 90, {min = 30, max = 160})
local blade_len = param("blade_length", 45, {min = 15, max = 90})
local segs = 24
for i = 0, segs - 1 do
  local z0, z1 = height * i / segs, height * (i + 1) / segs
  local r = 2.0 - 0.8 * (i + 0.5) / segs
  part("tower", extrude(profile.circle(r), vec3(0, 0, z0), vec3(0, 0, z1)), {ifc = "IfcColumn", predefined = "COLUMN"})
end
part("nacelle", extrude(profile.rect(3, 3), vec3(-3, 0, height), vec3(3, 0, height)))
local hub = vec3(4.5, 0, height)
local rotor_r = 1.5
local rotor = define_component("rotor", {
  extrude(profile.circle(rotor_r), vec3(-1.5, 0, 0), vec3(1.5, 0, 0)),
})
place(rotor, hub, "rotor")
local secs = 6
for b = 0, 2 do
  local a = b * 2 * math.pi / 3
  local dy, dz = math.sin(a), math.cos(a)
  local forms = {}
  for k = 0, secs - 1 do
    -- the root sits on the rotor surface, not inside it
    local r0 = rotor_r + blade_len * k / secs
    local r1 = rotor_r + blade_len * (k + 1) / secs
    local rad = 0.8 - 0.65 * k / secs
    forms[#forms + 1] = {
      name = "section" .. (k + 1),
      form = extrude(profile.circle(rad), vec3(0, dy * r0, dz * r0), vec3(0, dy * r1, dz * r1)),
    }
  end
  place(define_component("blade" .. (b + 1), forms), hub, "blade")
end
audit{strict = true}
