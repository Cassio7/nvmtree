# nvmtree

Formerly `nvmem` (renamed in v0.5.0 for findability: "nvmem" is the Linux/Zephyr
non-volatile memory subsystem). The repo is `Cassio7/nvmtree`; GitHub redirects
the old `Cassio7/nvmem` URLs. The local checkout folder may still be named
`nvmem`. Don't reintroduce the old name except in that history note.

A cross-platform (Linux, macOS, Windows) Rust CLI that inspects an nvm installation
([nvm-sh](https://github.com/nvm-sh/nvm) on Linux/macOS,
[nvm-windows](https://github.com/coreybutler/nvm-windows) on Windows):
it lists every installed Node.js version and the global packages
(`npm i -g ...`) installed for each of them.

## Status

Commands:

- `nvmtree [VERSION] [-t/--table] [-u/--updates] [--refresh]` (same as `nvmtree list`):
  versions + global packages as a tree (with each package's commands from `bin`,
  `(linked)` for `npm link`), or a package × version matrix. **Offline by
  default**; with `-u` (or `--refresh` = `-u` ignoring the cache) it also shows
  `↑ x.y.z` when npm has a newer version and `deprecated` + message (`↑` / `!`
  in the table), ending with a summary + `npm i -g <pkg>@latest` commands per
  version. `--offline` is a hidden no-op kept for v0.1.3 compatibility.
  `VERSION` filters: `24`, `22.9`, `v24.21.0`, `default`/`current`, `node`, or
  an nvm-sh alias like `lts/*`.
  `-u` also checks Node releases (`src/node.rs`): one update per release line
  (major; `0.minor` before 1.0), only on the newest installed version of the
  line (`↑ v24.22.2`, `(security)` if a release in between has security
  fixes). Older installed patches of a line are "superseded": no marker, or a
  bare dim `↑` when the newer one is filtered out of the view. The summary
  prints `nvm install X --reinstall-packages-from=Y` (nvm-windows:
  `nvm install X` + `nvmtree diff Y X`), `nvm uninstall` for superseded
  versions (never the default/current one; a `# has N packages ...` comment
  when the newer one lacks some), and LTS / Current lines newer than any
  installed (`nvm install --lts` / `node`; nvm-windows `lts` / `latest`;
  hidden with a VERSION filter). Planned over all installed versions, even
  when filtered.
- `nvmtree diff FROM TO [--exact] [--command-only]`: packages only in FROM / only in
  TO / at different versions, plus the command to install FROM's missing packages
  into TO (`nvm exec <v> npm i -g ...`; two lines on nvm-windows). A spec matching
  several versions uses the newest. Linked packages are left out of the command.
- `nvmtree which NAME`: versions that have a package, matched by package name or
  command name (`which tsc` → typescript). Always includes bundled packages.
  Exit code 1 with "did you mean" hints when nothing matches.
- `nvmtree npx [-u] [--refresh]`: packages in npm's npx cache (`src/npx.rs`;
  `$npm_config_cache/_npx`, default `~/.npm/_npx` / `%LOCALAPPDATA%\npm-cache\_npx`),
  most recently used first, with folder size and "used X ago" (folder mtime),
  total + `npm cache clean --force` hint. Not per Node version, so it's a
  separate view; the default tree ends with a one-line summary (first 3 names
  + `(+N more)`), hidden with a VERSION filter, `--table`, or an empty cache.
  `which` also reports npx-cached matches ("also cached by npx" / "not
  installed globally, but cached by npx", exit 0). Sizes are only computed in
  `nvmtree npx` (the summary only reads `package.json` files, stays instant).
- Global flags: `-a/--all` (include bundled npm/corepack), `--color auto|always|never`,
  `--json`. Global options work before or after the subcommand (don't bring
  back clap's `args_conflicts_with_subcommands`: it broke `nvmtree --all diff ...`).
- `--json` (`src/json.rs`, list/diff/which; `npx --json` is refused for now):
  one pretty JSON document on stdout via plain `println!`, never styled; errors
  stay on stderr (stdout empty). Offline unless `-u`. Dedicated `Serialize`
  structs are the public format, with `schema_version` (`json::SCHEMA_VERSION`,
  now 1): bump it on breaking changes (renamed/removed fields, changed
  meaning); new fields are fine. With `-u`, versions get `node_update`
  (`{version, security}`) / `superseded_by`, the document `node_releases`
  (`latest`, `latest_lts`, `lts_codename`) and `node_releases_unreachable`. `--json` conflicts with `diff --command-only`;
  `which` with no match prints the document (with `suggestions`) and exits 1.

## Design principles

- Plain, non-interactive CLI: print and exit.
- Read-only: never installs, deletes or changes anything; it prints commands
  (install, update, clean) for the user to run. The only files it writes are the
  registry and Node release caches, and only with `-u`. `--json` prints to stdout, never to a file.
- Offline by default; network only with `-u`/`--refresh`.
- Mark the nvm-sh `default` alias, or the active version on nvm-windows (`current`).
- Hide Node-bundled packages (`npm`, `corepack`) by default; `--all` shows them.
- Colors via `anstream`/`anstyle`: auto-disabled when piped, respects `NO_COLOR`.
- Registry checks (`src/registry.rs`, only with `-u`/`--refresh`, because they
  send package names/versions to the registry): per package, GET
  `<registry>/<name>/latest` and `<registry>/<name>/<installed>` (~5 KB each,
  scoped names as `@scope%2Fname`), in parallel (≤16 threads, 3 s timeout) via
  `ureq`. Cached 6 h in `registry.json` under the OS cache dir
  (`~/Library/Caches/nvmtree`, `$XDG_CACHE_HOME/nvmtree` or `~/.cache/nvmtree`,
  `%LOCALAPPDATA%\nvmtree\cache`); 404s are cached too; on network failure stale
  cache entries are used. Registry = `$npm_config_registry` or npmjs. Never
  block or fail the listing because of the registry.
- Node releases (only with `-u`): one GET of `<mirror>/index.json`
  (`$NVM_NODEJS_ORG_MIRROR` on nvm-sh, `node_mirror:` in nvm-windows'
  `settings.txt`, default `https://nodejs.org/dist`; 10 s timeout), in
  parallel with the registry checks. Sends nothing about the install. Stable
  releases only (prereleases dropped), versions rebuilt from their numeric
  parts before going into commands. Cached 6 h in `node-releases.json` next
  to `registry.json` (keyed by URL), stale cache on failure.
- Candidate next features: Homebrew tap, shell completions, `npx --json`, disk
  usage per Node version, CONTRIBUTING.md + PR/issue templates.

## Build & run

```bash
cargo build                 # debug build -> target/debug/nvmtree
cargo build --release       # optimized build -> target/release/nvmtree
cargo run                   # build + run
cargo run -- <args>         # pass arguments to the CLI
cargo install --path .      # install `nvmtree` into ~/.cargo/bin
```

## Quality checks

Run these before committing:

```bash
cargo fmt                   # format
cargo clippy --all-targets -- -D warnings # lint (same as CI), warnings are errors
cargo test                  # tests
```

Cross-compiling from macOS no longer works (`ureq` → rustls → `ring` has C
code needing a target C toolchain); CI's native Linux/macOS/Windows jobs are the
cross-platform check.

## CI / releases

- `.github/workflows/ci.yml` — on push to `main`/`master` and PRs: fmt, clippy, tests on Ubuntu, macOS, Windows;
  shellcheck `install.sh` and parse-check `install.ps1`.
- `.github/workflows/release.yml` — pushing a `v*` tag builds release binaries
  (Linux x64/arm64, macOS arm64/x64, Windows x64) and attaches them to a GitHub
  release, then `verify-install` installs that release with the real install
  scripts on Linux x64/arm64, macOS and Windows (pwsh + PowerShell 5.1).
- Release asset names have NO version (`nvmtree-<target>[.exe]`) so
  `releases/latest/download/<asset>` links always work. `install.sh` /
  `install.ps1` depend on these names — keep them in sync.
- Each release asset gets a `<asset>.sha256` file (`<hex>  <asset>`, sha256sum
  format) and a build provenance attestation (`actions/attest-build-provenance`;
  check with `gh attestation verify <file> --repo Cassio7/nvmtree`).
- `install.sh` / `install.ps1` verify the binary against `<asset>.sha256`:
  a mismatch is an error (nothing installed); a missing checksum (404, releases
  up to v0.1.3) or no hashing tool is only a warning.
- Actions are pinned to full commit SHAs with a version comment
  (`uses: actions/checkout@<sha> # v7.0.1`); Dependabot's github-actions updates
  bump them. `dtolnay/rust-toolchain` pins the `stable` branch head, so it needs
  `toolchain: stable` in `with:`.
- Token permissions are least-privilege: workflows default to
  `contents: read`; only the release `build` job gets `contents: write`,
  `id-token: write` and `attestations: write`.
- In `run:` blocks, never interpolate `${{ ... }}`; pass values through `env:`
  and use `"$VAR"` (expressions are fine in `runs-on`, `with:`, `if:`, `env:`).
- To release: bump `version` in `Cargo.toml`, commit (`chore: release vX.Y.Z`),
  push, then create an annotated tag whose message is the release notes
  (markdown bullets) and push it:
  `git tag -a vX.Y.Z --cleanup=verbatim -F notes.md && git push origin vX.Y.Z`.
  The release workflow uses the tag message as the GitHub release description
  (`--cleanup=verbatim` keeps lines starting with `#`).
- `.github/workflows/audit.yml` — `cargo audit` (RustSec advisories) weekly
  (Mondays), on dependency changes, and on demand.
- Keep tests portable: no `*` or other Windows-invalid chars in fixture file
  names; gate symlink tests with `#[cfg(unix)]`; on Windows a directory can't be
  opened with `File::open` (needs `FILE_FLAG_BACKUP_SEMANTICS`, see the
  `set_mtime` test helper in `src/npx.rs`).
- `main` has a GitHub ruleset ("protect main"); direct pushes to `main` are the
  normal workflow (no PRs required for the owner).

## Commit messages

[Conventional Commits](https://www.conventionalcommits.org): `<type>: <summary>`
(imperative, lowercase, no trailing period), optional body explaining why.
One logical change per commit; split mixed work into separate commits.

| Type | Use for |
|---|---|
| `feat:` | New user-facing behavior (commands, flags, output) |
| `fix:` | Bug fixes |
| `docs:` | README, CLAUDE.md, comments only |
| `refactor:` | Code changes with no behavior change |
| `test:` | Adding or fixing tests only |
| `build:` | `Cargo.toml`, dependencies, release profile |
| `ci:` | `.github/` workflows, Dependabot config |
| `chore:` | Version bumps (`chore: release vX.Y.Z`), housekeeping |

Breaking changes: `feat!:` / `fix!:` plus a `BREAKING CHANGE:` line in the body.

## Dependencies

Direct crates (see `Cargo.toml`): `clap` (CLI), `anstream`/`anstyle` (colors),
`anyhow` (errors), `serde`/`serde_json` (JSON), `ureq` (HTTPS to the npm
registry); `tempfile` for tests. `Cargo.lock` is committed (binary crate).

- `cargo update` — newest semver-compatible versions (only touches `Cargo.lock`).
- Major bumps (`4.x` → `5.0`): edit `Cargo.toml`, fix code, run quality checks.
- `cargo audit` (install: `cargo install cargo-audit`) — RustSec advisories;
  `cargo outdated --root-deps-only` (install: `cargo install cargo-outdated`).
- Dependabot (`.github/dependabot.yml`, free on GitHub): weekly PRs, Cargo
  minor/patch grouped into one PR (`build(deps): ...`), GitHub Actions grouped
  (`ci(deps): ...`). Merge once CI is green; majors need review. Dependency
  updates don't need a release unless they fix something user-facing (e.g. a
  security advisory in `ureq`/rustls → release a patch version).
- Prefer few dependencies: reading files/JSON covers most needs; add a crate
  only when it saves real complexity.

## Project layout

- `Cargo.toml` / `Cargo.lock` — package manifest (Rust edition 2024) and committed lockfile
- `README.md` — user-facing docs (keep short; details live in `--help`);
  `LICENSE` (MIT); `.gitignore` (`/target`, `/.claude/worktrees/` for agent worktrees)
- `.github/` — `workflows/{ci,release,audit}.yml`, `dependabot.yml` (see above)
- `install.sh` (POSIX sh, macOS/Linux) / `install.ps1` (Windows) — one-line
  installers served from `main` via raw.githubusercontent.com; download the
  release asset to `~/.local/bin` / `%LOCALAPPDATA%\Programs\nvmtree`.
  Env: `NVMTREE_VERSION`, `NVMTREE_INSTALL_DIR`.
- `src/main.rs` — CLI (clap derive, subcommands; bare `nvmtree` = `list`), dispatch.
  `cli_definition_is_valid` test runs clap's `debug_assert` — keep it.
- `src/compare.rs` — `diff`, `is_installable` / `not_installable`,
  `install_command` / `npm_install_command`, `find` / `suggestions` (pure, no I/O)
- `src/sanitize.rs` — guards for external text (folder names, `package.json`,
  registry): `is_safe_package_name` / `is_safe_version` gate everything that
  goes into generated shell commands or registry URLs (invalid ones are left
  out and reported, never quoted/escaped). `printable` strips control
  characters and bidi overrides; `output::styled`/`padded` apply it to all
  printed text — never write external text to the output without them.
- `src/json.rs` — `--json` documents (`list`, `diff`, `which`) + `print`
- `src/npx.rs` — npx cache: `cache_dir`, `packages` (reads `_npx/<hash>/package.json`
  `_npx.packages` specs, legacy `dependencies`; names that would escape the
  folder are ignored), `matching`, `dir_size` (doesn't follow symlinks)
- `src/node.rs` — Node release index: `releases` (fetch + cache, fetch fn
  as a parameter like the registry), `parse_index`, `plan` (per-line
  `Status::Update` / `Superseded`, `new_lines`); `output::node_summary` renders it
- `src/registry.rs` — npm registry lookups + cache; `lookup` takes the fetch
  function as a parameter so tests use a fake registry (no network in tests)
- `src/output.rs` — renderers returning styled `String`s: `tree`, `table`,
  `registry_summary`, `diff`, `found` (which), `npx_summary` / `npx_list` /
  `npx_found`, plus `human_size` / `ago` (tests strip ANSI with
  `anstream::adapter::strip_str`)
- `src/nvm.rs` — `Install` (nvm-sh or nvm-windows layout, chosen by `cfg!(windows)`):
  installed versions, default/current marker, global packages, `load_entries`,
  version-spec `select`; `read_package` (package.json → version, bins) is shared
  with `npx.rs`. Unit tests use fake nvm dirs via `tempfile`.

## Working on this repo

- Workflow: the owner tries changes locally (`cargo run -- ...`) before
  committing. Commit, push and release only when asked; release notes are
  markdown bullets in the annotated tag (see CI / releases).
- `gh` isn't installed locally. Check CI with GitHub's public REST API via
  `curl` (`/repos/Cassio7/nvmtree/actions/runs?head_sha=...`, `.../runs/<id>/jobs`,
  `.../check-runs/<job id>/annotations`, `.../releases/tags/<tag>`). Job logs need
  auth (HTTP 403), so diagnose failures from the failing step + code.
- Windows can't be built or tested locally: CI's Windows job (and the
  release's `verify-install` for `install.ps1`) is the only Windows check.

## How nvm-sh stores things (Linux/macOS)

- nvm root: `$NVM_DIR`, defaulting to `~/.nvm` when the variable is unset
  (it is often unset in non-interactive shells, so always fall back).
- Installed Node versions: `$NVM_DIR/versions/node/v<X.Y.Z>/`
- Global packages for a version: `$NVM_DIR/versions/node/v<X.Y.Z>/lib/node_modules/`
  - Scoped packages live one level deeper: `node_modules/@scope/<name>/`
  - Each package's version is in its `package.json` (`"version"` field)
  - `npm` and `corepack` ship with Node itself, they were not installed by the user
- Aliases (e.g. `default`): plain-text files in `$NVM_DIR/alias/`, content is a
  version spec like `24` or `v24.21.0` (may need resolving to an installed version).
- Prefer reading the filesystem directly over shelling out to `nvm`/`npm`:
  `nvm` is a shell function (not a binary) and spawning `npm ls -g` per version is slow.

## How nvm-windows stores things

- Home: `%NVM_HOME%`, defaulting to `%APPDATA%\nvm`. Contains `settings.txt`,
  `nvm.exe`, and (by default) the versions.
- `settings.txt` lines `root: <dir>` (where versions live, may differ from home)
  and `path: <dir>` (the active-version symlink).
- Versions: `<root>\v<X.Y.Z>\`; global packages: `<root>\v<X.Y.Z>\node_modules\`
  (no `lib/`).
- No aliases/default. The active version is wherever the symlink
  (`%NVM_SYMLINK%`, usually `C:\Program Files\nodejs`) points.
