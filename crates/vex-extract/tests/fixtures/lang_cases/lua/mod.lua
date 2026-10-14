local json = require("cjson")
local util = require "util.strings"
require("side_effect")

local M = {}

-- Public API.
function M.encode(v)
  return json.encode(v)
end

function M:method(x)
  return self.encode(x)
end

M.decode = function(s) return json.decode(s) end

local function private(a)
  return util.trim(a)
end

local helpers = {
  first = function(t) return t[1] end,
  limit = 10,
  nested = { deep = function() return private(1) end },
}

local a, b = function() end, function() end

return M
