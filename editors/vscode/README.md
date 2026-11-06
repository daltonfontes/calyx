# Calyx for VS Code

Diagnostics as you type, the outline of a file, go to definition, hover and
syntax colors for `.clyx` files. The checks are the ones `calyx check` runs,
from the language server in the `calyx` binary (`calyx lsp`).

## Install

1. Install calyx (see the [README](../../README.md)), and check that
   `calyx --version` works in a terminal.
2. Build the extension and install it:

   ```bash
   cd editors/vscode
   npm install
   npm run package          # makes calyx-<version>.vsix
   code --install-extension calyx-*.vsix
   ```

If `calyx` is not on the PATH VS Code sees, set **Calyx: Path** (`calyx.path`)
to the full path of the binary, e.g. `C:\Users\you\.calyx\bin\calyx.exe`.

## Other editors

Any editor with an LSP client can start `calyx lsp` (stdio) for `.clyx`
files. In Neovim, for example:

```lua
vim.filetype.add({ extension = { clyx = "calyx" } })
vim.api.nvim_create_autocmd("FileType", {
  pattern = "calyx",
  callback = function()
    vim.lsp.start({ name = "calyx", cmd = { "calyx", "lsp" } })
  end,
})
```
