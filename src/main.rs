mod compare;
mod engines;
mod json;
mod node;
mod npx;
mod nvm;
mod output;
mod registry;
mod sanitize;

use std::time::SystemTime;

use anyhow::{Context, Result, bail};
use clap::{Args, ColorChoice, Parser, Subcommand};

/// List installed nvm Node versions and their global npm packages.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    list: ListArgs,

    /// Also include packages bundled with Node (npm, corepack).
    #[arg(short, long, global = true)]
    all: bool,

    /// When to use colors.
    #[arg(long, global = true, value_enum, default_value_t = ColorChoice::Auto)]
    color: ColorChoice,

    /// Print one JSON document on stdout instead of text (list, diff, which).
    /// Still offline unless -u is given.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand)]
enum Command {
    /// List versions and their global packages (the default).
    List(ListArgs),

    /// Compare the global packages of two versions and print how to install
    /// the missing ones, e.g. `nvmtree diff 24 26`.
    Diff {
        /// Version to copy packages from.
        from: String,
        /// Version to copy packages to.
        to: String,
        /// Pin the versions currently installed in FROM (`name@x.y.z`).
        #[arg(long)]
        exact: bool,
        /// Only print the install command (e.g. to pipe into a shell).
        #[arg(long, conflicts_with = "json")]
        command_only: bool,
    },

    /// Show which versions have a package, by package or command name,
    /// e.g. `nvmtree which tsc`.
    Which {
        /// Package name (`typescript`, `@shopify/cli`) or command (`tsc`).
        name: String,
    },

    /// List packages cached by `npx` (not installed globally, shared by all
    /// Node versions), with their size and when npx last used them.
    Npx {
        /// Check the npm registry for updates and deprecated packages.
        #[arg(short, long)]
        updates: bool,

        /// Like --updates, but ignore cached registry answers.
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Args)]
struct ListArgs {
    /// Only show versions matching this: `24`, `22.9`, `v24.21.0`,
    /// `default`, `node`, or an nvm alias like `lts/*`.
    #[arg(value_name = "VERSION")]
    node_version: Option<String>,

    /// Show a package × version matrix instead of a tree.
    #[arg(short, long)]
    table: bool,

    /// Check the npm registry for updates and deprecated packages
    /// (sends package names and versions to the registry).
    #[arg(short, long)]
    updates: bool,

    /// Like --updates, but ignore cached registry answers and check again now.
    #[arg(long)]
    refresh: bool,

    /// No-op kept for compatibility: nvmtree is offline unless --updates is given.
    #[arg(long, hide = true)]
    offline: bool,
}

impl ListArgs {
    /// Whether to contact the npm registry. Offline is the default.
    fn check_updates(&self) -> bool {
        (self.updates || self.refresh) && !self.offline
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.color {
        ColorChoice::Auto => {}
        ColorChoice::Always => anstream::ColorChoice::Always.write_global(),
        ColorChoice::Never => anstream::ColorChoice::Never.write_global(),
    }

    let install = nvm::Install::detect()?;
    let json = cli.json;
    match cli.command {
        None => list(&install, &cli.list, cli.all, json),
        Some(Command::List(args)) => list(&install, &args, cli.all, json),
        Some(Command::Diff {
            from,
            to,
            exact,
            command_only,
        }) => diff(&install, &from, &to, exact, command_only, cli.all, json),
        // Bundled packages are always searchable: `nvmtree which npx` should work.
        Some(Command::Which { name }) => which(&install, &name, json),
        Some(Command::Npx { .. }) if json => {
            bail!("--json is supported for list, diff and which, not npx yet")
        }
        Some(Command::Npx { updates, refresh }) => npx(updates || refresh, refresh),
    }
}

fn list(install: &nvm::Install, args: &ListArgs, all: bool, json: bool) -> Result<()> {
    let installed = install.load_entries(all)?;
    if installed.is_empty() && !json {
        anstream::println!("No Node versions installed in {}", install.root.display());
        return Ok(());
    }
    let mut entries = match &args.node_version {
        Some(spec) => select(install, &installed, spec)?
            .into_iter()
            .map(|i| installed[i].clone())
            .collect(),
        None => installed.clone(),
    };

    // The release index and the registry are independent: ask both at once.
    let (incomplete, node_plan) = if args.check_updates() {
        std::thread::scope(|s| {
            let releases = s.spawn(|| {
                node::releases(
                    &node::index_url(install),
                    node::default_cache_path().as_deref(),
                    registry::CACHE_TTL,
                    args.refresh,
                    node::http_fetch,
                )
            });
            let incomplete = check_registry(
                entries.iter_mut().flat_map(|e| {
                    let node = Some(e.version.parts);
                    e.packages.iter_mut().map(move |p| (p, node))
                }),
                args.refresh,
            );
            let releases = releases.join().ok().flatten();
            // Planned over every installed version, so a filter can't hide
            // that a newer patch of the same line is installed.
            let plan = node::plan(&installed, releases.as_deref());
            for entry in &mut entries {
                entry.node = plan.status.get(&entry.version.name).cloned();
            }
            (incomplete, Some((plan, releases.is_some())))
        })
    } else {
        (false, None)
    };

    if json {
        return json::print(&json::list(
            install,
            &entries,
            args.check_updates(),
            incomplete,
            node_plan.as_ref().map(|(plan, reached)| (plan, *reached)),
        ));
    }
    let rendered = if args.table {
        output::table(&entries)
    } else {
        output::tree(&entries)
    };
    anstream::print!("{rendered}");
    if let Some((plan, reached)) = &node_plan {
        // New lines are about the whole install, not a filtered view.
        let new_lines = match args.node_version {
            Some(_) => &[][..],
            None => &plan.new_lines[..],
        };
        anstream::print!(
            "{}",
            output::node_summary(&entries, new_lines, &install.flavor, !reached)
        );
    }
    anstream::print!(
        "{}",
        output::registry_summary(&entries, &install.flavor, incomplete)
    );
    // The npx cache isn't tied to a version: only on the full tree view.
    if args.node_version.is_none() && !args.table {
        let cached = npx::cache_dir()
            .map(|dir| npx::packages(&dir))
            .unwrap_or_default();
        anstream::print!("{}", output::npx_summary(&cached));
    }
    Ok(())
}

fn npx(check_updates: bool, refresh: bool) -> Result<()> {
    let dir = npx::cache_dir().context("can't locate the npm cache (set npm_config_cache)")?;
    let mut cached = npx::packages(&dir);
    let incomplete =
        check_updates && check_registry(cached.iter_mut().map(|p| (&mut p.package, None)), refresh);
    let dirs: Vec<&std::path::Path> = cached.iter().map(|p| p.dir.as_path()).collect();
    let sizes = npx::dir_sizes(&dirs);
    anstream::print!(
        "{}",
        output::npx_list(&cached, &sizes, &tilde(&dir), SystemTime::now())
    );
    if incomplete {
        anstream::print!(
            "{}",
            output::registry_summary(&[], &nvm::Flavor::NvmSh, true)
        );
    }
    Ok(())
}

/// `path` with the home folder shown as `~`.
fn tilde(path: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    match home.and_then(|h| path.strip_prefix(h).ok().map(|rest| rest.to_path_buf())) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
        None => path.display().to_string(),
    }
}

/// Fills in update / deprecation info from the npm registry, for packages
/// paired with the Node version they run on (`None`: no engine check).
/// Returns true when the registry couldn't be reached for some packages.
fn check_registry<'a>(
    packages: impl Iterator<Item = (&'a mut nvm::GlobalPackage, Option<(u64, u64, u64)>)>,
    refresh: bool,
) -> bool {
    let mut targets: Vec<_> = packages.collect();
    // Linked packages are local checkouts, not registry releases.
    let packages: Vec<registry::Query> = targets
        .iter()
        .filter(|(p, _)| !p.linked)
        .filter_map(|(p, node)| Some((p.name.clone(), p.version.clone()?, *node)))
        .collect();
    if packages.is_empty() {
        return false;
    }
    let lookup = registry::lookup(
        &packages,
        registry::default_cache_path().as_deref(),
        registry::CACHE_TTL,
        refresh,
        registry::http_fetcher(),
    );
    for (pkg, node) in targets.iter_mut() {
        if let Some(version) = &pkg.version
            && let Some(info) = lookup.info.get(&(pkg.name.clone(), version.clone(), *node))
        {
            pkg.registry = info.clone();
        }
    }
    lookup.incomplete
}

fn diff(
    install: &nvm::Install,
    from: &str,
    to: &str,
    exact: bool,
    command_only: bool,
    all: bool,
    json: bool,
) -> Result<()> {
    let entries = install.load_entries(all)?;
    // A spec can match several versions (e.g. `22`); use the newest.
    let from = &entries[select(install, &entries, from)?[0]];
    let to = &entries[select(install, &entries, to)?[0]];

    let d = compare::diff(&from.packages, &to.packages);
    let command = compare::install_command(&install.flavor, &to.version, &d.only_from, exact);
    if command_only {
        if let Some(command) = command {
            println!("{command}");
        }
        return Ok(());
    }
    let skipped = compare::not_installable(&d.only_from, exact);
    if json {
        return json::print(&json::diff(from, to, &d, command.as_deref(), &skipped));
    }
    anstream::print!(
        "{}",
        output::diff(from, to, &d, command.as_deref(), &skipped)
    );
    Ok(())
}

fn which(install: &nvm::Install, name: &str, json: bool) -> Result<()> {
    let entries = install.load_entries(true)?;
    let found = compare::find(&entries, name);
    let cached = npx::cache_dir()
        .map(|dir| npx::packages(&dir))
        .unwrap_or_default();
    let in_npx = npx::matching(&cached, name);
    if json {
        let suggestions = if found.is_empty() && in_npx.is_empty() {
            compare::suggestions(&entries, name)
        } else {
            Vec::new()
        };
        let doc = json::which(name, &found, &in_npx, suggestions);
        json::print(&doc)?;
        // Nothing found: the document (with suggestions) is still printed.
        if doc.is_empty() {
            std::process::exit(1);
        }
        return Ok(());
    }
    let npx_note = output::npx_found(&in_npx, !found.is_empty(), SystemTime::now());
    if found.is_empty() && !in_npx.is_empty() {
        anstream::print!("{npx_note}");
        return Ok(());
    }
    if found.is_empty() {
        let name = sanitize::printable(name);
        let hints: Vec<_> = compare::suggestions(&entries, &name)
            .into_iter()
            .map(sanitize::printable)
            .collect();
        if hints.is_empty() {
            bail!("no package or command named '{name}' in any Node version");
        }
        bail!(
            "no package or command named '{name}'. Did you mean: {}?",
            hints.join(", ")
        );
    }
    anstream::print!("{}{npx_note}", output::found(&found));
    Ok(())
}

/// Like [`nvm::Install::select`], but an error when nothing matches.
fn select(install: &nvm::Install, entries: &[nvm::Entry], spec: &str) -> Result<Vec<usize>> {
    let selected = install.select(entries, spec);
    if selected.is_empty() {
        let installed: Vec<&str> = entries.iter().map(|e| e.version.name.as_str()).collect();
        bail!(
            "no installed version matches '{spec}' (installed: {})",
            installed.join(", ")
        );
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn global_options_work_before_subcommands() {
        let cli = Cli::try_parse_from(["nvmtree", "--color", "never", "which", "tsc"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Which { .. })));
        let cli = Cli::try_parse_from(["nvmtree", "--all", "diff", "24", "26"]).unwrap();
        assert!(cli.all);
        assert!(matches!(cli.command, Some(Command::Diff { .. })));
        // The list shorthand still works.
        let cli = Cli::try_parse_from(["nvmtree", "--all", "24"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.list.node_version.as_deref(), Some("24"));
    }

    #[test]
    fn parses_list_shorthand_and_subcommands() {
        let cli = Cli::try_parse_from(["nvmtree", "24", "--table"]).unwrap();
        assert!(cli.command.is_none());
        assert_eq!(cli.list.node_version.as_deref(), Some("24"));
        assert!(cli.list.table);

        // Offline unless asked; --refresh implies --updates; --offline still parses.
        assert!(
            !Cli::try_parse_from(["nvmtree"])
                .unwrap()
                .list
                .check_updates()
        );
        assert!(
            Cli::try_parse_from(["nvmtree", "-u"])
                .unwrap()
                .list
                .check_updates()
        );
        assert!(
            Cli::try_parse_from(["nvmtree", "24", "--updates", "--table"])
                .unwrap()
                .list
                .check_updates()
        );
        assert!(
            Cli::try_parse_from(["nvmtree", "--refresh"])
                .unwrap()
                .list
                .check_updates()
        );
        assert!(
            !Cli::try_parse_from(["nvmtree", "-u", "--offline"])
                .unwrap()
                .list
                .check_updates()
        );
        assert!(
            Cli::try_parse_from(["nvmtree", "list", "-u"])
                .unwrap()
                .command
                .is_some_and(|c| matches!(c, Command::List(args) if args.check_updates()))
        );

        assert!(matches!(
            Cli::try_parse_from(["nvmtree", "npx", "-u"])
                .unwrap()
                .command,
            Some(Command::Npx {
                updates: true,
                refresh: false
            })
        ));

        // --json is global: before or after the subcommand.
        assert!(Cli::try_parse_from(["nvmtree", "--json"]).unwrap().json);
        assert!(
            Cli::try_parse_from(["nvmtree", "24", "--json", "-u"])
                .unwrap()
                .json
        );
        assert!(
            Cli::try_parse_from(["nvmtree", "diff", "24", "26", "--json"])
                .unwrap()
                .json
        );
        assert!(
            Cli::try_parse_from(["nvmtree", "--json", "which", "tsc"])
                .unwrap()
                .json
        );
        assert!(
            Cli::try_parse_from(["nvmtree", "diff", "24", "26", "--json", "--command-only"])
                .is_err()
        );

        let cli = Cli::try_parse_from(["nvmtree", "diff", "24", "26", "--all"]).unwrap();
        assert!(cli.all);
        assert!(matches!(cli.command, Some(Command::Diff { .. })));

        let cli = Cli::try_parse_from(["nvmtree", "which", "tsc"]).unwrap();
        assert!(matches!(cli.command, Some(Command::Which { name }) if name == "tsc"));
    }
}
