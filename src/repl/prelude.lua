-- Longe REPL prelude: value formatting and state serialization.
-- Everything here is a baseline global; user state is whatever is added later.

local function sorted_keys(t)
  local ks = {}
  for k in pairs(t) do ks[#ks + 1] = k end
  table.sort(ks, function(a, b)
    local ta, tb = type(a), type(b)
    if ta == tb and (ta == "number" or ta == "string") then return a < b end
    return ta < tb or (ta == tb and tostring(a) < tostring(b))
  end)
  return ks
end

-- Human-readable rendering, depth and size limited.
function __longe_fmt(v, depth, seen)
  depth = depth or 0
  seen = seen or {}
  local tv = type(v)
  if tv == "string" then return string.format("%q", v) end
  if tv ~= "table" then return tostring(v) end
  if seen[v] then return "<cycle>" end
  if depth >= 4 then return "{...}" end
  seen[v] = true
  local parts, n = {}, 0
  local is_array = #v > 0
  for _, k in ipairs(sorted_keys(v)) do
    n = n + 1
    if n > 200 then parts[#parts + 1] = "... (" .. tostring(#sorted_keys(v)) .. " entries)"; break end
    local val = __longe_fmt(v[k], depth + 1, seen)
    if is_array and type(k) == "number" then parts[#parts + 1] = val
    elseif type(k) == "string" and k:match("^[%a_][%w_]*$") then parts[#parts + 1] = k .. " = " .. val
    else parts[#parts + 1] = "[" .. __longe_fmt(k, depth + 1, seen) .. "] = " .. val end
  end
  seen[v] = nil
  return "{" .. table.concat(parts, ", ") .. "}"
end

-- Serialize a value as Lua source. Functions become bytecode (upvalues are lost),
-- cycles become nil, userdata and threads are skipped.
local function ser(v, depth, seen, out)
  local tv = type(v)
  if tv == "nil" or tv == "boolean" or tv == "number" then out[#out + 1] = tostring(v); return true end
  if tv == "string" then out[#out + 1] = string.format("%q", v); return true end
  if tv == "function" then
    local ok, bc = pcall(string.dump, v, true)
    if not ok then return false end
    local esc = bc:gsub(".", function(ch) return string.format("\\%03d", ch:byte()) end)
    out[#out + 1] = "load(\"" .. esc .. "\", '=state', 'b')"
    return true
  end
  if tv ~= "table" then return false end
  if seen[v] or depth > 32 then out[#out + 1] = "nil"; return true end
  seen[v] = true
  out[#out + 1] = "{"
  for _, k in ipairs(sorted_keys(v)) do
    local kt = type(k)
    if kt == "string" or kt == "number" or kt == "boolean" then
      local mark = #out
      out[#out + 1] = "["
      ser(k, depth + 1, seen, out)
      out[#out + 1] = "]="
      if ser(v[k], depth + 1, seen, out) then out[#out + 1] = "," else
        for i = #out, mark + 1, -1 do out[i] = nil end
      end
    end
  end
  out[#out + 1] = "}"
  seen[v] = nil
  return true
end

-- Dump every non-baseline global as `name = value` lines.
function __longe_dump()
  local out = {}
  for _, k in ipairs(sorted_keys(_G)) do
    if type(k) == "string" and not __longe_baseline[k] and k:match("^[%a_][%w_]*$") then
      local buf = {}
      if ser(_G[k], 0, {}, buf) then
        out[#out + 1] = k .. " = " .. table.concat(buf)
      end
    end
  end
  return table.concat(out, "\n") .. "\n"
end

-- Record the baseline after the bindings are installed.
function __longe_freeze()
  __longe_baseline = {}
  for k in pairs(_G) do __longe_baseline[k] = true end
  __longe_baseline["_last"] = true
end
