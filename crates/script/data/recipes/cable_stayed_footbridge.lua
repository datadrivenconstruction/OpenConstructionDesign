-- Cable-stayed footbridge: H-pylon off-centre, fan stays to both girders, deck slabs and railings.
local span = param("span", "80 m", {min = "30 m", max = "200 m"})
local width = param("width", "4 m", {min = "2.5 m", max = "8 m"})
bridge{kind = "cable_stayed", span = span, width = width, deck_z = "6 m", bays = 16,
       pylon_h = "30 m", pylon_at = 0.3, name = "footbridge"}
audit{strict = true}
