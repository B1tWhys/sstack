-- Runs inside nvim once the person's config has loaded, and turns it into an editing engine for
-- one piece of text. Nvim pushes every change from here rather than being asked, because a request
-- would hang for as long as nvim waits on a key, such as the one after `f`.
local api, fn = vim.api, vim.fn
local text, cursor = ...
local namespace = api.nvim_create_namespace("sstack")

-- Returns the channel to the process that embedded this nvim.
local function embedder()
  for _, candidate in ipairs(api.nvim_list_chans()) do
    if candidate.mode == "rpc" and candidate.stream == "stdio" then
      return candidate.id
    end
  end
end

local channel = embedder()

-- The latest error, which lasts until the next key.
local message = nil

local MODES = {
  i = "insert",
  R = "replace",
  c = "command",
  v = "visual",
  V = "visual",
  s = "visual",
  S = "visual",
  ["\22"] = "visual",
  ["\19"] = "visual",
}

-- What getregionpos calls each kind of selection. Select mode selects the same way as Visual.
local REGIONS = { s = "v", S = "V", ["\19"] = "\22" }

-- The kinds of message that mean something went wrong.
local ERRORS = { emsg = true, echoerr = true, lua_error = true, rpc_error = true, wmsg = true }

-- Returns the byte offset each line starts at, in the text those lines join into.
local function line_starts(lines)
  local starts, offset = {}, 0
  for index, line in ipairs(lines) do
    starts[index] = offset
    offset = offset + #line + 1
  end
  return starts
end

-- Returns the byte ranges selected in Visual mode, one for each line that has any.
local function selection(mode, starts)
  if MODES[mode:sub(1, 1)] ~= "visual" then
    return {}
  end
  local ranges = {}
  local kind = REGIONS[mode] or mode
  for _, region in ipairs(fn.getregionpos(fn.getpos("v"), fn.getpos("."), { type = kind })) do
    local first, last = region[1], region[2]
    if first[3] > 0 then
      local start = starts[first[2]]
      table.insert(ranges, { start = start + first[3] - 1, ["end"] = start + last[3] })
    end
  end
  return ranges
end

-- Returns what nvim's number options put beside each line, every label as wide as the number
-- column, or nothing if they put nothing there. Options are read each time so that `:set` works.
local function line_numbers(count, current)
  local numbered, relative = vim.wo.number, vim.wo.relativenumber
  if not (numbered or relative) then
    return {}
  end
  local width = fn.getwininfo(api.nvim_get_current_win())[1].textoff - 1
  local labels = {}
  for line = 1, count do
    local distance = math.abs(line - current)
    local on_cursor = distance == 0
    local shown = (relative and not (on_cursor and numbered)) and distance or line
    local align = (relative and numbered and on_cursor) and "-" or ""
    labels[line] = string.format("%" .. align .. width .. "d ", shown)
  end
  return labels
end

-- Returns the command line being typed, if there is one.
local function cmdline()
  local kind = fn.getcmdtype()
  if kind == "" then
    return nil
  end
  return { text = kind .. fn.getcmdline(), cursor = fn.getcmdpos() }
end

local function push()
  local lines = api.nvim_buf_get_lines(0, 0, -1, true)
  local starts = line_starts(lines)
  local position = api.nvim_win_get_cursor(0)
  local mode = fn.mode(1)
  vim.rpcnotify(channel, "sstack_state", {
    text = table.concat(lines, "\n"),
    cursor = starts[position[1]] + position[2],
    mode = MODES[mode:sub(1, 1)] or "normal",
    selection = selection(mode, starts),
    line_numbers = line_numbers(#lines, position[1]),
    cmdline = cmdline(),
    message = message,
  })
end

-- Fills the buffer as if it had been opened that way, so undo cannot empty it.
local function load_draft()
  local lines = vim.split(text, "\n", { plain = true })
  local undolevels = vim.o.undolevels
  vim.o.undolevels = -1
  api.nvim_buf_set_lines(0, 0, -1, true, lines)
  vim.o.undolevels = undolevels
  vim.bo.modified = false
  local starts = line_starts(lines)
  local row = #lines
  while starts[row] > cursor do
    row = row - 1
  end
  local column = cursor - starts[row]
  api.nvim_win_set_cursor(0, { row, column })
  -- Sent as input because a command run from here only takes effect after the next key.
  api.nvim_input(column >= #lines[row] and "<Cmd>startinsert!<CR>" or "<Cmd>startinsert<CR>")
end

-- Makes screen motions such as `gj` match how the text is drawn, whatever the person's config
-- chose for their own windows. The number options stay theirs, because the labels are drawn too.
local function match_the_drawn_prompt()
  vim.bo.swapfile = false
  vim.o.laststatus = 0
  vim.o.showtabline = 0
  vim.o.cmdheight = 0
  vim.wo.statuscolumn = ""
  vim.wo.signcolumn = "no"
  vim.wo.foldcolumn = "0"
  vim.wo.list = false
  vim.wo.wrap = true
  vim.wo.linebreak = false
  vim.wo.breakindent = false
  vim.wo.foldenable = false
  vim.wo.conceallevel = 0
  vim.wo.showbreak = ""
end

function _G.sstack_submit()
  local lines = api.nvim_buf_get_lines(0, 0, -1, true)
  vim.rpcnotify(channel, "sstack_submitted", table.concat(lines, "\n"))
  api.nvim_buf_set_lines(0, 0, -1, true, {})
  vim.cmd.startinsert()
end

-- A buffer of its own, because the current one may be a dashboard or a restored file.
api.nvim_set_current_buf(api.nvim_create_buf(false, true))
match_the_drawn_prompt()
load_draft()

-- Enter in Normal mode submits. The mapping is local to the buffer, so it wins over any the
-- person's config made for Enter, and `nowait` stops a longer mapping that starts with Enter from
-- holding it back.
vim.keymap.set("n", "<CR>", _G.sstack_submit, {
  buffer = true,
  nowait = true,
  desc = "Submit to sstack",
})

api.nvim_create_autocmd({
  "ModeChanged",
  "CursorMoved",
  "CursorMovedI",
  "TextChanged",
  "TextChangedI",
  "TextChangedP",
  "CmdlineChanged",
}, { callback = push })

api.nvim_create_autocmd("OptionSet", {
  pattern = { "number", "relativenumber", "numberwidth" },
  callback = push,
})

vim.ui_attach(namespace, { ext_messages = true }, function(event, kind, content)
  if event == "msg_show" and ERRORS[kind] then
    local chunks = vim.tbl_map(function(chunk)
      return chunk[2]
    end, content)
    message = table.concat(chunks)
    vim.schedule(push)
  end
end)

vim.on_key(function()
  if message then
    message = nil
    vim.schedule(push)
  end
end, namespace)

push()
