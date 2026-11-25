-- Cloister walk: a pointed vault of steel ribs and glass beside an arcade of free pointed arches.
local length = param("length", "40 m", {min = "8 m", max = "120 m"})
vault{kind = "pointed", span = "8 m", rise = "7 m", length = length, arches = 11,
      segments = 12, panels = "12 mm", name = "cloister"}
for i = 0, 10 do
  local y = length * i / 10
  arch{from = vec3(9.5, y, 0), to = vec3(13.5, y, 0), kind = "pointed", rise = "5 m",
       segments = 8, profile = "SHS 200x10", name = "arcade"}
end
audit{strict = true}
