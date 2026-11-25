-- Truss footbridge: Pratt trusses either side of the deck with top bracing.
local span = param("span", "36 m", {min = "12 m", max = "90 m"})
bridge{kind = "truss", span = span, width = "3.5 m", depth = "3 m", bays = 9,
       deck_z = "4 m", name = "truss bridge"}
audit{strict = true}
