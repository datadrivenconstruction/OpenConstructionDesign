-- Pagoda: tiers of shrinking plan, columns and ring beams, a curled-eave hip roof per tier, a finial.
local tiers = param("tiers", 5, {min = 1, max = 15})
local base = param("base_width", "10 m", {min = "3 m", max = "60 m"})
local th = param("tier_height", "4 m", {min = "2.5 m", max = "12 m"})
local curl = param("eave_curl", "0.6 m", {min = "0 m", max = "2.5 m"})
pagoda{tiers = tiers, base_width = base, tier_height = th, eave_curl = curl,
       taper = 0.8, overhang = "1.5 m", finial = "3 m", name = "pagoda"}
audit{strict = true}
