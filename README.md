nvmtree
=======

> see what's installed in every node version — and copy it to the next one.

Each Node version managed by [nvm](https://github.com/nvm-sh/nvm) or [nvm-windows](https://github.com/coreybutler/nvm-windows) has its own global packages. Install a new Node and your `tsc`, `pnpm`, `shopify` are gone. `nvmtree` lists them per version, flags updates, and prints the command to move them over.

Read-only, offline by default, single binary.

Install
-------

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/Cassio7/nvmtree/main/install.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/Cassio7/nvmtree/main/install.ps1 | iex
```

Run it again to update. Or grab a binary from [releases](https://github.com/Cassio7/nvmtree/releases/latest), or `cargo install --git https://github.com/Cassio7/nvmtree`.

Usage
-----

```bash
nvmtree                  # every version and its global packages
nvmtree 24               # only matching versions
nvmtree --table          # package × version matrix
nvmtree -u               # also check for package and Node updates (online)

nvmtree diff 24 26       # what 24 has that 26 doesn't + the command to install it
nvmtree which tsc        # which versions have it
nvmtree npx              # what npx has cached, and how to clean it

nvmtree --json           # JSON output (list, diff, which)
```

`nvmtree --help` for the rest.

Privacy
-------

No network unless you pass `-u`. With `-u`, your global package names and versions go to the npm registry (or `$npm_config_registry`), and Node's release list is fetched from nodejs.org (or your nvm mirror). Answers are cached for 6 hours.

Not supported: fnm, Volta, n, asdf, mise.

Uninstall
---------

```bash
rm ~/.local/bin/nvmtree   # or: cargo uninstall nvmtree
```

Windows: delete `%LOCALAPPDATA%\Programs\nvmtree` and remove it from your `PATH`.

License
-------

[MIT](LICENSE)
