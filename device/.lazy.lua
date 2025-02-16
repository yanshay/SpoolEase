-- https://rust-analyzer.github.io/manual.html#configuration
return {
  "mrcjkb/rustaceanvim",
  opts = {
    server = {
      default_settings = {
        -- rust-analyzer language server configuration
        ["rust-analyzer"] = {
          server = {
            -- extraEnv = { {["RA_LOG"]="project_model=debug"} }, -- For debugging rust-analyzer, to see log location do :LspInfo in neovim
          },
          cargo = {
            allFeatures = false, -- important
            extraArgs = {"--release"}, -- probably not required, but better since used for building
            -- allTargets = false,  -- Not required
            target = "xtensa-esp32-none-elf", -- can be avoided if using .cargo/config.toml as described below. CAN'T be a list even if in rust-analyzer doc it says otherwise
          },
        },
      },
    },
  },
}

-- working version

-- return {
--   "mrcjkb/rustaceanvim",
--   -- version = "^4", -- Recommended
--   -- ft = { "rust" },
--   opts = {
--     server = {
--       -- on_attach = function(_, bufnr)
--       --   vim.keymap.set("n", "<leader>cR", function()
--       --     vim.cmd.RustLsp("codeAction")
--       --   end, { desc = "Code Action", buffer = bufnr })
--       --   vim.keymap.set("n", "<leader>dr", function()
--       --     vim.cmd.RustLsp("debuggables")
--       --   end, { desc = "Rust Debuggables", buffer = bufnr })
--       -- end,
--       default_settings = {
--         -- rust-analyzer language server configuration
--         ["rust-analyzer"] = {
--           server = {
--             -- extraEnv = { {["RA_LOG"]="project_model=debug"} }, -- For debugging rust-analyzer, to see log location do :LspInfo in neovim
--           },
--           cargo = {
--             allFeatures = false, -- important
--             -- loadOutDirsFromCheck = true,
--             -- buildScripts = {
--             --   enable = true,
--             -- },
--             extraArgs = {"--release"}, -- probably not required, but better since used for building
--             allTargets = false,  -- Not required
--             -- target = "xtensa-esp32-none-elf", -- can be avoided if using .cargo/config.toml as described below. CAN'T be a list even if in rust-analyzer doc it says otherwise
--           },
--           -- Add clippy lints for Rust.
--           -- checkOnSave = true,
--           -- procMacro = {
--           --   enable = true,
--           --   ignored = {
--           --     ["async-trait"] = { "async_trait" },
--           --     ["napi-derive"] = { "napi" },
--           --     ["async-recursion"] = { "async_recursion" },
--           --   },
--           -- },
--         },
--       },
--     },
--   },
--   -- config = function(_, opts)
--   --   vim.g.rustaceanvim = vim.tbl_deep_extend("keep", vim.g.rustaceanvim or {}, opts or {})
--   --   LazyVim.info(tprint(vim.g.rustaceanvim))
--   --   if vim.fn.executable("rust-analyzer") == 0 then
--   --     LazyVim.error(
--   --       "**rust-analyzer** not found in PATH, please install it.\nhttps://rust-analyzer.github.io/",
--   --       { title = "rustaceanvim" }
--   --     )
--   --   end
--   -- end,
-- }
