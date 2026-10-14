local M = {}
M.sub = { f = function() return 1 end }
local t = { ["key"] = function() return 2 end, [1] = function() end }
return M
