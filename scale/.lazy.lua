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
            workspace = false,
            allFeatures = false, -- important
            extraArgs = {"--release"} -- , "-p", "device"}, -- probably not required, but better since used for building
            -- allTargets = false,  -- Not required
            -- target = "xtensa-esp32-none-elf", -- can be avoided if using .cargo/config.toml as described below. CAN'T be a list even if in rust-analyzer doc it says otherwise
          },
        },
      },
    },
  },
}
