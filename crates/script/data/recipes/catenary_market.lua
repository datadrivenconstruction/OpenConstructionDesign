-- Market hall: catenary steel arches, purlins and glazing, the shape of a hanging chain turned over.
local span = param("span", "24 m", {min = "10 m", max = "60 m"})
local rise = param("rise", "12 m", {min = "4 m", max = "30 m"})
local length = param("length", "48 m", {min = "12 m", max = "200 m"})
vault{kind = "catenary", span = span, rise = rise, length = length, arches = 9,
      segments = 16, panels = "12 mm", name = "market"}
audit{strict = true}
