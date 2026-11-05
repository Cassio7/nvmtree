//! Renders versions and packages as styled text. Output contains ANSI codes;
//! print it through `anstream`, which strips them when color is off.

use std::collections::BTreeSet;
use std::fmt::Write;
use std::time::SystemTime;

use anstyle::{AnsiColor, Style};

use crate::compare::{Diff, Found, is_installable, npm_install_command};
use crate::node::{Release, Status};
use crate::npx::NpxPackage;
use crate::nvm::{Entry, Flavor, GlobalPackage};
use crate::sanitize::printable;

const HEADER: Style = Style::new().bold();
const MARK: Style = AnsiColor::Green.on_default().bold();
const VERSION: Style = AnsiColor::Cyan.on_default();
const DIM: Style = Style::new().dimmed();
const WARN: Style = AnsiColor::Yellow.on_default();
const MISSING: Style = AnsiColor::Red.on_default();
/// Node release updates, distinct from package updates (`WARN`).
const NODE_UPDATE: Style = AnsiColor::Magenta.on_default();

/// `text` styled and left-aligned in `width` columns (padding stays unstyled).
/// Like [`styled`], strips terminal control characters from `text`.
fn padded(style: Style, text: &str, width: usize) -> String {
    let text = printable(text);
    let pad = width.saturating_sub(text.chars().count());
    format!("{style}{text}{style:#}{:pad$}", "")
}

/// `text` wrapped in `style`. All text in the output goes through here or
/// [`padded`], so control characters from package.json files or the registry
/// (which could rewrite the terminal) are always stripped.
fn styled(style: Style, text: &str) -> String {
    format!("{style}{}{style:#}", printable(text))
}

fn version_cell(version: Option<&str>) -> (Style, &str) {
    match version {
        Some(v) => (VERSION, v),
        None => (MISSING, "?"),
    }
}

/// Cells laid out in aligned columns separated by two spaces. Columns whose
/// width is 0 are skipped; trailing empty cells produce no padding.
fn columns(cells: &[(Style, String)], widths: &[usize]) -> String {
    let last = cells.iter().rposition(|(_, text)| !text.is_empty());
    let mut parts = Vec::new();
    for (i, ((style, text), &width)) in cells.iter().zip(widths).enumerate() {
        if width == 0 || last.is_none_or(|last| i > last) {
            continue;
        }
        parts.push(if Some(i) == last {
            styled(*style, text)
        } else {
            padded(*style, text, width)
        });
    }
    parts.join("  ")
}

/// The per-package cells of a tree line: name, version, update, deprecated,
/// linked, commands, and what the registry's `latest` needs when it doesn't
/// run on this Node version.
fn package_cells(pkg: &GlobalPackage) -> Vec<(Style, String)> {
    let (version_style, version) = version_cell(pkg.version.as_deref());
    let text = |cond: bool, s: String| if cond { s } else { String::new() };
    vec![
        (Style::new(), pkg.name.clone()),
        (version_style, version.to_string()),
        (
            WARN,
            pkg.registry
                .update
                .as_ref()
                .map_or_else(String::new, |v| format!("↑ {v}")),
        ),
        (
            MISSING,
            text(pkg.registry.deprecated.is_some(), "deprecated".into()),
        ),
        (WARN, text(pkg.linked, "(linked)".into())),
        (
            DIM,
            text(!pkg.bins.is_empty(), format!("→ {}", pkg.bins.join(", "))),
        ),
        (
            NODE_UPDATE,
            pkg.registry
                .latest_needs_node
                .as_ref()
                .map_or_else(String::new, needs_node_note),
        ),
    ]
}

/// `(⬆ 22.2.6 needs Node ≥22.22.3)`: the minimum newer Node satisfying the
/// latest version's `engines.node`, or the range itself when there is none.
fn needs_node_note(needs: &crate::registry::NeedsNode) -> String {
    let node = match needs.min_node {
        Some(v) => format!("≥{}", plain_version(v)),
        None => needs.engines_node.clone(),
    };
    format!("(⬆ {} needs Node {node})", needs.latest)
}

/// Each version followed by a tree of its packages.
pub fn tree(entries: &[Entry]) -> String {
    let mut out = String::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out += &version_heading(entry);
        match node_marker(entry, entries) {
            NodeMarker::Update { to, security } => {
                out += &format!("  {}", styled(NODE_UPDATE, &format!("↑ {to}")));
                if security {
                    out += &format!(" {}", styled(MISSING, "(security)"));
                }
            }
            NodeMarker::Hidden => out += &format!("  {}", styled(DIM, "↑")),
            NodeMarker::None => {}
        }
        out.push('\n');

        let packages = &entry.packages;
        if packages.is_empty() {
            let _ = writeln!(
                out,
                "  {} {}",
                styled(DIM, "└─"),
                styled(DIM, "(no global packages)")
            );
            continue;
        }

        let rows: Vec<Vec<(Style, String)>> = packages.iter().map(package_cells).collect();
        let widths: Vec<usize> = (0..rows[0].len())
            .map(|col| {
                rows.iter()
                    .map(|r| r[col].1.chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        for (j, (pkg, row)) in packages.iter().zip(&rows).enumerate() {
            let last = j + 1 == packages.len();
            let branch = if last { "└─" } else { "├─" };
            let _ = writeln!(out, "  {} {}", styled(DIM, branch), columns(row, &widths));
            if let Some(message) = &pkg.registry.deprecated {
                let stem = if last { " " } else { "│" };
                let _ = writeln!(out, "  {}    {}", styled(DIM, stem), styled(DIM, message));
            }
        }
    }
    out
}

/// A matrix: one row per package, one column per Node version.
pub fn table(entries: &[Entry]) -> String {
    let names: BTreeSet<&str> = entries
        .iter()
        .flat_map(|e| e.packages.iter().map(|p| p.name.as_str()))
        .collect();
    if names.is_empty() {
        return styled(DIM, "(no global packages)") + "\n";
    }

    // Plain-text header labels; the marked version gets a `*`, a newer Node
    // release (or a newer installed patch that isn't shown) a `↑`.
    let headers: Vec<String> = entries
        .iter()
        .map(|e| {
            let mut header = e.version.name.clone();
            if e.label.is_some() {
                header.push('*');
            }
            if node_marker(e, entries) != NodeMarker::None {
                header.push_str(" ↑");
            }
            header
        })
        .collect();

    // Version, then `↑` for an available update and `!` for deprecated.
    let cell = |entry: &Entry, name: &str| -> (Style, String) {
        match entry.packages.iter().find(|p| p.name == name) {
            Some(p) => {
                let (mut style, v) = version_cell(p.version.as_deref());
                let mut text = v.to_string();
                if p.registry.update.is_some() {
                    text.push_str(" ↑");
                    style = WARN;
                }
                if p.registry.deprecated.is_some() {
                    text.push_str(" !");
                    style = MISSING;
                }
                (style, text)
            }
            None => (DIM, "-".to_string()),
        }
    };

    let first_width = names
        .iter()
        .map(|n| n.chars().count())
        .chain(["package".len()])
        .max()
        .unwrap_or(0);
    let widths: Vec<usize> = entries
        .iter()
        .zip(&headers)
        .map(|(entry, header)| {
            names
                .iter()
                .map(|n| cell(entry, n).1.chars().count())
                .chain([header.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();

    let mut out = String::new();
    let mut row = vec![padded(HEADER, "package", first_width)];
    for ((entry, header), &width) in entries.iter().zip(&headers).zip(&widths) {
        let style = if entry.label.is_some() { MARK } else { HEADER };
        row.push(padded(style, header, width));
    }
    out += row.join("  ").trim_end();
    out.push('\n');

    for name in &names {
        let mut row = vec![padded(Style::new(), name, first_width)];
        for (entry, &width) in entries.iter().zip(&widths) {
            let (style, text) = cell(entry, name);
            row.push(padded(style, &text, width));
        }
        out += row.join("  ").trim_end();
        out.push('\n');
    }

    let packages = || entries.iter().flat_map(|e| &e.packages);
    let mut legend = Vec::new();
    if let Some(label) = entries.iter().find_map(|e| e.label) {
        legend.push(format!("* {label}"));
    }
    if packages().any(|p| p.registry.update.is_some())
        || entries
            .iter()
            .any(|e| node_marker(e, entries) != NodeMarker::None)
    {
        legend.push("↑ update available".to_string());
    }
    if packages().any(|p| p.registry.deprecated.is_some()) {
        legend.push("! deprecated".to_string());
    }
    if !legend.is_empty() {
        out += &styled(DIM, &legend.join("   "));
        out.push('\n');
    }
    out
}

#[derive(Debug, PartialEq, Eq)]
enum NodeMarker {
    None,
    /// A newer release of this line exists.
    Update {
        to: String,
        security: bool,
    },
    /// A newer patch of this line is installed but filtered out of the view:
    /// a bare `↑` (when it's shown, its own marker says it all).
    Hidden,
}

fn node_marker(entry: &Entry, shown: &[Entry]) -> NodeMarker {
    match &entry.node {
        Some(Status::Update { to, security }) => NodeMarker::Update {
            to: to.clone(),
            security: *security,
        },
        Some(Status::Superseded { by, .. }) if !shown.iter().any(|e| e.version.name == *by) => {
            NodeMarker::Hidden
        }
        _ => NodeMarker::None,
    }
}

/// `24.22.1` from a version's numeric parts (safe in a shell command).
fn plain_version(parts: (u64, u64, u64)) -> String {
    format!("{}.{}.{}", parts.0, parts.1, parts.2)
}

/// Node updates (one per release line), older patches that a newer
/// installed one supersedes, and release lines newer than any installed.
/// `unreachable` when the release list couldn't be loaded.
pub fn node_summary(
    entries: &[Entry],
    new_lines: &[Release],
    flavor: &Flavor,
    unreachable: bool,
) -> String {
    let mut out = String::new();
    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    let command = |line: &str| format!("  {}\n", styled(MARK, line));
    let comment = |text: &str| format!("  {}", styled(DIM, &format!("# {text}")));

    let updates: Vec<(&Entry, &str, bool)> = entries
        .iter()
        .filter_map(|e| match &e.node {
            Some(Status::Update { to, security }) => Some((e, to.as_str(), *security)),
            _ => None,
        })
        .collect();
    if !updates.is_empty() {
        let how = match flavor {
            Flavor::NvmSh => "installs it and copies the global packages",
            Flavor::NvmWindows { .. } => "then copy the global packages with nvmtree diff",
        };
        let _ = writeln!(
            out,
            "\n{} available. To update ({how}):",
            styled(
                NODE_UPDATE,
                &format!("↑ {}", plural(updates.len(), "Node update"))
            )
        );
        for (entry, to, security) in updates {
            let from = plain_version(entry.version.parts);
            let Some(to) = crate::nvm::parse_version(to).map(plain_version) else {
                continue;
            };
            let note = if security {
                format!("  {}", styled(MISSING, "# security release"))
            } else {
                String::new()
            };
            match flavor {
                Flavor::NvmSh => {
                    let line = format!("nvm install {to} --reinstall-packages-from={from}");
                    let _ = writeln!(out, "  {}{note}", styled(MARK, &line));
                }
                Flavor::NvmWindows { .. } => {
                    let _ = writeln!(
                        out,
                        "  {}{note}",
                        styled(MARK, &format!("nvm install {to}"))
                    );
                    out += &command(&format!("nvmtree diff {from} {to}"));
                }
            }
        }
    }

    // The version nvm uses isn't suggested for removal.
    let superseded: Vec<(&Entry, &str, usize)> = entries
        .iter()
        .filter(|e| e.label.is_none())
        .filter_map(|e| match &e.node {
            Some(Status::Superseded { by, missing }) => Some((e, by.as_str(), *missing)),
            _ => None,
        })
        .collect();
    if !superseded.is_empty() {
        let _ = writeln!(
            out,
            "\n{} of a line with a newer patch installed. To remove:",
            styled(
                DIM,
                &if superseded.len() == 1 {
                    "1 older version".to_string()
                } else {
                    format!("{} older versions", superseded.len())
                }
            )
        );
        for (entry, by, missing) in superseded {
            let old = plain_version(entry.version.parts);
            let _ = write!(out, "  {}", styled(MARK, &format!("nvm uninstall {old}")));
            if missing > 0 {
                let by = by.trim_start_matches('v');
                out += &comment(&format!(
                    "has {} v{by} doesn't: nvmtree diff {old} {by}",
                    plural(missing, "package")
                ));
            }
            out.push('\n');
        }
    }

    if !new_lines.is_empty() {
        let names: Vec<String> = new_lines
            .iter()
            .map(|r| match &r.lts {
                Some(codename) => format!("{} (LTS {codename})", r.version),
                None => format!("{} (Current)", r.version),
            })
            .collect();
        let _ = writeln!(
            out,
            "\n{} {}, not installed. To install:",
            styled(
                NODE_UPDATE,
                if new_lines.len() == 1 {
                    "New Node line:"
                } else {
                    "New Node lines:"
                }
            ),
            styled(Style::new(), &names.join(", "))
        );
        for release in new_lines {
            let line = match (flavor, release.lts.is_some()) {
                (Flavor::NvmSh, true) => "nvm install --lts",
                (Flavor::NvmSh, false) => "nvm install node",
                (Flavor::NvmWindows { .. }, true) => "nvm install lts",
                (Flavor::NvmWindows { .. }, false) => "nvm install latest",
            };
            out += &command(line);
        }
    }

    if unreachable {
        let _ = writeln!(
            out,
            "\n{}",
            styled(
                DIM,
                "Couldn't download the Node release list, Node updates were skipped."
            )
        );
    }
    out
}

/// Totals of available updates and deprecated packages, with the commands
/// to update each Node version. Empty when everything is up to date.
pub fn registry_summary(entries: &[Entry], flavor: &Flavor, incomplete: bool) -> String {
    let mut out = String::new();
    let updates: usize = entries
        .iter()
        .flat_map(|e| &e.packages)
        .filter(|p| p.registry.update.is_some())
        .count();
    let deprecated: Vec<String> = entries
        .iter()
        .flat_map(|e| {
            e.packages
                .iter()
                .filter(|p| p.registry.deprecated.is_some())
                .map(move |p| format!("{} in {}", p.name, e.version.name))
        })
        .collect();

    let plural = |n: usize, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    if updates > 0 {
        let _ = writeln!(
            out,
            "\n{} available. To update:",
            styled(WARN, &format!("↑ {}", plural(updates, "update")))
        );
        for entry in entries {
            let specs: Vec<String> = entry
                .packages
                .iter()
                .filter(|p| is_installable(p, false))
                .filter_map(|p| {
                    let update = p.registry.update.as_ref()?;
                    Some(if p.registry.update_is_latest() {
                        format!("{}@latest", p.name)
                    } else {
                        // `latest` needs a newer Node: the newest that fits.
                        format!("{}@{update}", p.name)
                    })
                })
                .collect();
            if specs.is_empty() {
                continue;
            }
            for line in npm_install_command(flavor, &entry.version, &specs).lines() {
                let _ = writeln!(out, "  {}", styled(MARK, line));
            }
        }
    }
    if !deprecated.is_empty() {
        let _ = writeln!(
            out,
            "{}{}: {}",
            if updates > 0 { "" } else { "\n" },
            styled(
                MISSING,
                &format!("! {} deprecated", plural(deprecated.len(), "package"))
            ),
            styled(Style::new(), &deprecated.join(", "))
        );
    }
    if incomplete {
        let _ = writeln!(
            out,
            "\n{}",
            styled(
                DIM,
                "Couldn't reach the npm registry, some update checks were skipped."
            )
        );
    }
    out
}

fn version_heading(entry: &Entry) -> String {
    let mut out = styled(HEADER, &entry.version.name);
    if let Some(label) = entry.label {
        out += &format!(" {}", styled(MARK, &format!("({label})")));
    }
    out
}

fn package_lines(out: &mut String, packages: &[&GlobalPackage]) {
    let width = packages.iter().map(|p| p.name.chars().count()).max();
    for pkg in packages {
        let (style, version) = version_cell(pkg.version.as_deref());
        let _ = write!(
            out,
            "  {}  {}",
            padded(Style::new(), &pkg.name, width.unwrap_or(0)),
            styled(style, version)
        );
        if pkg.linked {
            out.push_str(&format!("  {}", styled(WARN, "(linked)")));
        }
        out.push('\n');
    }
}

/// The differences between two versions, plus how to install what `to` lacks.
pub fn diff(
    from: &Entry,
    to: &Entry,
    d: &Diff,
    install_command: Option<&str>,
    skipped: &[&GlobalPackage],
) -> String {
    let mut out = format!(
        "{} {} {}\n",
        version_heading(from),
        styled(DIM, "→"),
        version_heading(to)
    );

    if d.only_from.is_empty() && d.only_to.is_empty() && d.changed.is_empty() {
        out += &format!(
            "\nNo differences ({} identical package{}).\n",
            d.same,
            if d.same == 1 { "" } else { "s" }
        );
        return out;
    }

    for (packages, entry) in [(&d.only_from, from), (&d.only_to, to)] {
        if !packages.is_empty() {
            let _ = writeln!(out, "\nOnly in {}:", styled(HEADER, &entry.version.name));
            package_lines(&mut out, packages);
        }
    }

    if !d.changed.is_empty() {
        out += "\nDifferent versions:\n";
        let width = d.changed.iter().map(|(f, _)| f.name.chars().count()).max();
        for (f, t) in &d.changed {
            let (fs, fv) = version_cell(f.version.as_deref());
            let (ts, tv) = version_cell(t.version.as_deref());
            let _ = writeln!(
                out,
                "  {}  {} {} {}",
                padded(Style::new(), &f.name, width.unwrap_or(0)),
                styled(fs, fv),
                styled(DIM, "→"),
                styled(ts, tv)
            );
        }
    }

    if d.same > 0 {
        let _ = writeln!(
            out,
            "\n{}",
            styled(
                DIM,
                &format!(
                    "{} identical package{} not shown.",
                    d.same,
                    if d.same == 1 { "" } else { "s" }
                )
            )
        );
    }

    let linked: Vec<&str> = d
        .only_from
        .iter()
        .filter(|p| p.linked)
        .map(|p| p.name.as_str())
        .collect();
    if let Some(command) = install_command {
        let _ = writeln!(
            out,
            "\nTo install the missing packages in {}:",
            styled(HEADER, &to.version.name)
        );
        for line in command.lines() {
            let _ = writeln!(out, "  {}", styled(MARK, line));
        }
    }
    if !linked.is_empty() {
        let _ = writeln!(
            out,
            "{}",
            styled(
                WARN,
                &format!(
                    "Linked packages are not included, re-run `npm link` for: {}",
                    linked.join(", ")
                )
            )
        );
    }
    if !skipped.is_empty() {
        let names: Vec<&str> = skipped.iter().map(|p| p.name.as_str()).collect();
        let _ = writeln!(
            out,
            "{}",
            styled(
                WARN,
                &format!(
                    "Not included, the package name or version looks invalid: {}",
                    names.join(", ")
                )
            )
        );
    }
    out
}

/// Where each found package is installed and where it is missing.
pub fn found(found: &[Found]) -> String {
    let mut out = String::new();
    for (i, f) in found.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out += &styled(HEADER, f.name);
        if !f.bins.is_empty() {
            out += &format!("  {}", styled(DIM, &format!("→ {}", f.bins.join(", "))));
        }
        out.push('\n');

        let headings: Vec<String> = f
            .installed
            .iter()
            .map(|(e, _)| version_heading(e))
            .collect();
        let width = f
            .installed
            .iter()
            .map(|(e, _)| {
                let label = e.label.map_or(0, |l| l.len() + 3);
                e.version.name.chars().count() + label
            })
            .max()
            .unwrap_or(0);
        for ((entry, pkg), heading) in f.installed.iter().zip(&headings) {
            let visible =
                entry.version.name.chars().count() + entry.label.map_or(0, |l| l.len() + 3);
            let (style, version) = version_cell(pkg.version.as_deref());
            let _ = write!(
                out,
                "  {heading}{:pad$}  {}",
                "",
                styled(style, version),
                pad = width - visible
            );
            if pkg.linked {
                out += &format!("  {}", styled(WARN, "(linked)"));
            }
            out.push('\n');
        }
        if !f.missing.is_empty() {
            let missing: Vec<String> = f.missing.iter().map(|e| version_heading(e)).collect();
            let _ = writeln!(out, "  {} {}", styled(DIM, "not in:"), missing.join(", "));
        }
    }
    out
}

/// `bytes` as a short human-readable size: `512 B`, `5.6 MB`, `1.2 GB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if value < 10.0 {
        format!("{value:.1} {}", UNITS[unit])
    } else {
        format!("{value:.0} {}", UNITS[unit])
    }
}

/// How long ago `secs` seconds is, roughly: `just now`, `5 minutes ago`, `2 days ago`.
pub fn ago(secs: u64) -> String {
    let (n, unit) = match secs {
        0..60 => return "just now".to_string(),
        60..3_600 => (secs / 60, "minute"),
        3_600..86_400 => (secs / 3_600, "hour"),
        86_400..2_592_000 => (secs / 86_400, "day"),
        2_592_000..31_536_000 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

fn used_ago(pkg: &NpxPackage, now: SystemTime) -> String {
    pkg.used
        .and_then(|t| now.duration_since(t).ok())
        .map_or_else(String::new, |d| format!("used {}", ago(d.as_secs())))
}

/// One line at the end of the default list pointing at `nvmtree npx`, or
/// nothing when the npx cache is empty.
pub fn npx_summary(packages: &[NpxPackage]) -> String {
    let mut names: Vec<&str> = Vec::new();
    for p in packages {
        if !names.contains(&p.package.name.as_str()) {
            names.push(&p.package.name);
        }
    }
    if names.is_empty() {
        return String::new();
    }
    const SHOWN: usize = 3;
    let mut list = names[..names.len().min(SHOWN)].join(", ");
    if names.len() > SHOWN {
        list += &format!(" (+{} more)", names.len() - SHOWN);
    }
    format!(
        "\n{} {}  {}\n",
        styled(DIM, "npx cache:"),
        styled(Style::new(), &list),
        styled(DIM, "· details: nvmtree npx")
    )
}

/// The full npx cache view: every cached package with its size and when npx
/// last used it. `sizes[i]` is the size of `packages[i].dir`.
pub fn npx_list(packages: &[NpxPackage], sizes: &[u64], location: &str, now: SystemTime) -> String {
    let mut out = format!(
        "{} {}\n",
        styled(HEADER, "npx cache"),
        styled(DIM, &format!("({location}, shared by all Node versions)"))
    );
    if packages.is_empty() {
        let _ = writeln!(out, "  {} {}", styled(DIM, "└─"), styled(DIM, "(empty)"));
        return out;
    }

    let rows: Vec<Vec<(Style, String)>> = packages
        .iter()
        .zip(sizes)
        .map(|(p, &size)| {
            let mut cells = package_cells(&p.package);
            cells.pop(); // needs a newer Node: no Node version for npx
            let bins = cells.pop().unwrap_or((DIM, String::new()));
            cells.pop(); // (linked): never for npx
            cells.push((DIM, human_size(size)));
            cells.push((DIM, used_ago(p, now)));
            cells.push(bins);
            cells
        })
        .collect();
    let widths: Vec<usize> = (0..rows[0].len())
        .map(|col| {
            rows.iter()
                .map(|r| r[col].1.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for (j, (p, row)) in packages.iter().zip(&rows).enumerate() {
        let last = j + 1 == packages.len();
        let branch = if last { "└─" } else { "├─" };
        let _ = writeln!(out, "  {} {}", styled(DIM, branch), columns(row, &widths));
        if let Some(message) = &p.package.registry.deprecated {
            let stem = if last { " " } else { "│" };
            let _ = writeln!(out, "  {}    {}", styled(DIM, stem), styled(DIM, message));
        }
    }

    // Several packages can share one cache folder: count each folder once.
    let mut seen = Vec::new();
    let total: u64 = packages
        .iter()
        .zip(sizes)
        .filter(|(p, _)| {
            let new = !seen.contains(&&p.dir);
            seen.push(&p.dir);
            new
        })
        .map(|(_, &s)| s)
        .sum();
    let count = packages.len();
    let _ = writeln!(
        out,
        "\n{} {}",
        styled(
            Style::new(),
            &format!(
                "{count} package{}, {}",
                if count == 1 { "" } else { "s" },
                human_size(total)
            )
        ),
        styled(
            DIM,
            "· clean: npm cache clean --force (clears all of npm's cache)"
        )
    );

    let updates: Vec<&str> = packages
        .iter()
        .filter(|p| p.package.registry.update.is_some())
        .map(|p| p.package.name.as_str())
        .collect();
    if !updates.is_empty() {
        let _ = writeln!(
            out,
            "{} {}",
            styled(WARN, &format!("↑ newer on npm: {}", updates.join(", "))),
            styled(DIM, "· npx <package>@latest fetches the newest")
        );
    }
    out
}

/// What `nvmtree which` adds about npx: where the npx cache has `matches`.
/// `installed_globally` tells whether global copies were already listed.
pub fn npx_found(matches: &[&NpxPackage], installed_globally: bool, now: SystemTime) -> String {
    if matches.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if installed_globally {
        out.push('\n');
    }
    let heading = if installed_globally {
        "also cached by npx:"
    } else {
        "not installed globally, but cached by npx:"
    };
    let _ = writeln!(out, "{}", styled(DIM, heading));
    for p in matches {
        let (style, version) = version_cell(p.package.version.as_deref());
        let _ = writeln!(
            out,
            "  {}  {}  {}",
            styled(HEADER, &p.package.name),
            styled(style, version),
            styled(DIM, &used_ago(p, now))
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvm::{GlobalPackage, NodeVersion};
    use std::path::PathBuf;

    fn pkg(name: &str, version: Option<&str>, bins: &[&str], linked: bool) -> GlobalPackage {
        GlobalPackage {
            name: name.to_string(),
            version: version.map(str::to_string),
            linked,
            bins: bins.iter().map(|b| b.to_string()).collect(),
            ..Default::default()
        }
    }

    fn entry(name: &str, label: Option<&'static str>, packages: Vec<GlobalPackage>) -> Entry {
        Entry {
            version: NodeVersion {
                name: name.to_string(),
                parts: crate::nvm::parse_version(name).unwrap(),
                path: PathBuf::new(),
            },
            label,
            packages,
            node: None,
        }
    }

    fn sample() -> Vec<Entry> {
        vec![
            entry(
                "v26.1.0",
                None,
                vec![pkg(
                    "typescript",
                    Some("5.9.2"),
                    &["tsc", "tsserver"],
                    false,
                )],
            ),
            entry(
                "v24.2.0",
                Some("default"),
                vec![
                    pkg("@shopify/cli", Some("3.80.0"), &["shopify"], false),
                    pkg("mytool", None, &[], true),
                    pkg("yoinks", Some("0.3.1"), &[], false),
                ],
            ),
            entry("v18.0.0", None, vec![]),
        ]
    }

    fn plain(s: &str) -> String {
        anstream::adapter::strip_str(s).to_string()
    }

    #[test]
    fn renders_tree() {
        assert_eq!(
            plain(&tree(&sample())),
            "\
v26.1.0
  └─ typescript  5.9.2  → tsc, tsserver

v24.2.0 (default)
  ├─ @shopify/cli  3.80.0            → shopify
  ├─ mytool        ?       (linked)
  └─ yoinks        0.3.1

v18.0.0
  └─ (no global packages)
"
        );
    }

    /// `sample()` with registry info: an update for @shopify/cli in v24 and a
    /// deprecated yoinks.
    fn sample_with_registry() -> Vec<Entry> {
        let mut entries = sample();
        entries[1].packages[0].registry.update = Some("4.9.3".into());
        entries[1].packages[2].registry.deprecated = Some("use something else".into());
        entries
    }

    #[test]
    fn renders_updates_limited_by_node_version() {
        let mut entries = sample();
        entries[0].packages[0].registry.update = Some("5.9.9".into());
        entries[0].packages[0].registry.latest_needs_node = Some(crate::registry::NeedsNode {
            latest: "7.0.2".into(),
            engines_node: ">=27".into(),
            min_node: Some((27, 0, 0)),
        });
        let entries = &entries[..1];
        assert_eq!(
            plain(&tree(entries)),
            "\
v26.1.0
  └─ typescript  5.9.2  ↑ 5.9.9  → tsc, tsserver  (⬆ 7.0.2 needs Node ≥27.0.0)
"
        );
        assert!(
            plain(&registry_summary(entries, &Flavor::NvmSh, false))
                .contains("nvm exec 26.1.0 npm i -g typescript@5.9.9")
        );
    }

    #[test]
    fn renders_tree_with_registry_info() {
        let entries = sample_with_registry();
        assert_eq!(
            plain(&tree(&entries[1..2])),
            "\
v24.2.0 (default)
  ├─ @shopify/cli  3.80.0  ↑ 4.9.3                        → shopify
  ├─ mytool        ?                            (linked)
  └─ yoinks        0.3.1            deprecated
       use something else
"
        );
    }

    #[test]
    fn renders_table_with_registry_info() {
        let out = plain(&table(&sample_with_registry()));
        assert!(out.contains("@shopify/cli  -        3.80.0 ↑"), "{out}");
        assert!(out.contains("yoinks        -        0.3.1 !"), "{out}");
        assert!(
            out.ends_with("* default   ↑ update available   ! deprecated\n"),
            "{out}"
        );
    }

    /// Two 24.x patches (one update, on the newest), an up-to-date 22.x and
    /// the planned statuses, as `node::plan` would give them.
    fn sample_with_node_updates() -> Vec<Entry> {
        let mut entries = vec![
            entry(
                "v24.22.1",
                None,
                vec![pkg("typescript", Some("5.9.2"), &[], false)],
            ),
            entry(
                "v24.22.0",
                None,
                vec![
                    pkg("typescript", Some("5.9.2"), &[], false),
                    pkg("pnpm", Some("10.0.0"), &[], false),
                ],
            ),
            entry("v24.21.0", Some("default"), vec![]),
            entry("v22.21.1", None, vec![]),
        ];
        entries[0].node = Some(Status::Update {
            to: "v24.22.2".into(),
            security: true,
        });
        entries[1].node = Some(Status::Superseded {
            by: "v24.22.1".into(),
            missing: 1,
        });
        entries[2].node = Some(Status::Superseded {
            by: "v24.22.1".into(),
            missing: 0,
        });
        entries
    }

    #[test]
    fn renders_node_updates_once_per_line() {
        let entries = sample_with_node_updates();
        let out = plain(&tree(&entries));
        assert!(
            out.starts_with("v24.22.1  ↑ v24.22.2 (security)\n"),
            "{out}"
        );
        // Older patches don't repeat the update while the newest is shown.
        assert!(out.contains("\nv24.22.0\n"), "{out}");
        assert!(out.contains("\nv24.21.0 (default)\n"), "{out}");
        // Filtered to an old patch: just a bare arrow.
        assert_eq!(
            plain(&tree(&entries[1..2])).lines().next(),
            Some("v24.22.0  ↑")
        );

        let out = plain(&table(&entries));
        assert!(
            out.starts_with("package     v24.22.1 ↑  v24.22.0  v24.21.0*  v22.21.1"),
            "{out}"
        );
        assert!(out.contains("↑ update available"), "{out}");
    }

    #[test]
    fn renders_node_summary() {
        let entries = sample_with_node_updates();
        let lts = Release {
            version: "v26.13.0".into(),
            lts: Some("Lithium".into()),
            security: false,
        };
        let current = Release {
            version: "v27.1.0".into(),
            lts: None,
            security: false,
        };
        let new_lines = [current, lts];
        assert_eq!(
            plain(&node_summary(&entries, &new_lines, &Flavor::NvmSh, false)),
            "
↑ 1 Node update available. To update (installs it and copies the global packages):
  nvm install 24.22.2 --reinstall-packages-from=24.22.1  # security release

1 older version of a line with a newer patch installed. To remove:
  nvm uninstall 24.22.0  # has 1 package v24.22.1 doesn't: nvmtree diff 24.22.0 24.22.1

New Node lines: v27.1.0 (Current), v26.13.0 (LTS Lithium), not installed. To install:
  nvm install node
  nvm install --lts
"
        );
        let windows = Flavor::NvmWindows { symlink: None };
        let out = plain(&node_summary(
            &entries[..1],
            &new_lines[1..],
            &windows,
            true,
        ));
        assert_eq!(
            out,
            "
↑ 1 Node update available. To update (then copy the global packages with nvmtree diff):
  nvm install 24.22.2  # security release
  nvmtree diff 24.22.1 24.22.2

New Node line: v26.13.0 (LTS Lithium), not installed. To install:
  nvm install lts

Couldn't download the Node release list, Node updates were skipped.
"
        );
        assert_eq!(node_summary(&entries[3..], &[], &Flavor::NvmSh, false), "");
    }

    #[test]
    fn renders_registry_summary() {
        let entries = sample_with_registry();
        assert_eq!(
            plain(&registry_summary(&entries, &Flavor::NvmSh, false)),
            "\n↑ 1 update available. To update:\n  \
             nvm exec 24.2.0 npm i -g @shopify/cli@latest\n\
             ! 1 package deprecated: yoinks in v24.2.0\n"
        );
        // Up to date: nothing printed.
        assert_eq!(registry_summary(&sample(), &Flavor::NvmSh, false), "");
        assert!(
            plain(&registry_summary(&sample(), &Flavor::NvmSh, true))
                .contains("Couldn't reach the npm registry")
        );
    }

    #[test]
    fn renders_table() {
        assert_eq!(
            plain(&table(&sample())),
            "\
package       v26.1.0  v24.2.0*  v18.0.0
@shopify/cli  -        3.80.0    -
mytool        -        ?         -
typescript    5.9.2    -         -
yoinks        -        0.3.1     -
* default
"
        );
    }

    #[test]
    fn table_without_packages() {
        let entries = vec![entry("v18.0.0", None, vec![])];
        assert_eq!(plain(&table(&entries)), "(no global packages)\n");
    }

    #[test]
    fn renders_diff() {
        let entries = sample();
        let (to, from) = (&entries[0], &entries[1]);
        let d = crate::compare::diff(&from.packages, &to.packages);
        let out = diff(
            from,
            to,
            &d,
            Some("nvm exec 26.1.0 npm i -g @shopify/cli yoinks"),
            &[],
        );
        assert_eq!(
            plain(&out),
            "\
v24.2.0 (default) → v26.1.0

Only in v24.2.0:
  @shopify/cli  3.80.0
  mytool        ?  (linked)
  yoinks        0.3.1

Only in v26.1.0:
  typescript  5.9.2

To install the missing packages in v26.1.0:
  nvm exec 26.1.0 npm i -g @shopify/cli yoinks
Linked packages are not included, re-run `npm link` for: mytool
"
        );
    }

    #[test]
    fn diff_reports_packages_left_out_of_the_command() {
        let to = &sample()[2];
        let from = entry(
            "v20.0.0",
            None,
            vec![pkg("evil;name", Some("1.0.0"), &[], false)],
        );
        let d = crate::compare::diff(&from.packages, &to.packages);
        let skipped = crate::compare::not_installable(&d.only_from, false);
        let out = plain(&diff(&from, to, &d, None, &skipped));
        assert!(!out.contains("To install"), "{out}");
        assert!(
            out.ends_with("Not included, the package name or version looks invalid: evil;name\n"),
            "{out}"
        );
    }

    #[test]
    fn renders_identical_diff() {
        let entries = sample();
        let d = crate::compare::diff(&entries[1].packages, &entries[1].packages);
        let out = diff(&entries[1], &entries[1], &d, None, &[]);
        assert!(plain(&out).ends_with("No differences (3 identical packages).\n"));
    }

    #[test]
    fn renders_found() {
        let mut entries = sample();
        entries[1]
            .packages
            .push(pkg("typescript", Some("5.0.0"), &["tsc"], false));
        let out = found(&crate::compare::find(&entries, "tsc"));
        assert_eq!(
            plain(&out),
            "\
typescript  → tsc, tsserver
  v26.1.0            5.9.2
  v24.2.0 (default)  5.0.0
  not in: v18.0.0
"
        );
    }

    fn npx_pkg(name: &str, version: &str, dir: &str, secs_ago: u64) -> NpxPackage {
        NpxPackage {
            package: pkg(name, Some(version), &[], false),
            dir: PathBuf::from(dir),
            used: Some(
                SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000 - secs_ago),
            ),
        }
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000)
    }

    #[test]
    fn formats_sizes_and_ages() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(5_872_026), "5.6 MB");
        assert_eq!(human_size(48 * 1024 * 1024), "48 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024 / 2), "1.5 GB");
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(7_200), "2 hours ago");
        assert_eq!(ago(86_400 * 3), "3 days ago");
        assert_eq!(ago(86_400 * 95), "3 months ago");
        assert_eq!(ago(86_400 * 800), "2 years ago");
    }

    #[test]
    fn npx_summary_lists_a_few_names() {
        assert_eq!(npx_summary(&[]), "");
        let few = [npx_pkg("skills", "1.7.2", "a", 60)];
        assert_eq!(
            plain(&npx_summary(&few)),
            "\nnpx cache: skills  · details: nvmtree npx\n"
        );
        let many: Vec<_> = ["skills", "vite", "skills", "prettier", "cowsay", "tsx"]
            .iter()
            .map(|n| npx_pkg(n, "1.0.0", n, 60))
            .collect();
        assert_eq!(
            plain(&npx_summary(&many)),
            "\nnpx cache: skills, vite, prettier (+2 more)  · details: nvmtree npx\n"
        );
    }

    #[test]
    fn renders_npx_list() {
        let mut cached = vec![
            npx_pkg("skills", "1.7.2", "/c/aaa", 7_200),
            npx_pkg("@scope/tool", "2.1.0", "/c/bbb", 86_400 * 3),
            npx_pkg("cowsay", "1.6.0", "/c/bbb", 86_400 * 3),
        ];
        cached[0].package.bins = vec!["add-skill".into(), "skills".into()];
        cached[0].package.registry.update = Some("1.8.0".into());
        let sizes = [5_872_026, 2048, 2048];
        assert_eq!(
            plain(&npx_list(&cached, &sizes, "~/.npm/_npx", now())),
            "\
npx cache (~/.npm/_npx, shared by all Node versions)
  ├─ skills       1.7.2  ↑ 1.8.0  5.6 MB  used 2 hours ago  → add-skill, skills
  ├─ @scope/tool  2.1.0           2.0 KB  used 3 days ago
  └─ cowsay       1.6.0           2.0 KB  used 3 days ago

3 packages, 5.6 MB · clean: npm cache clean --force (clears all of npm's cache)
↑ newer on npm: skills · npx <package>@latest fetches the newest
"
        );
        assert_eq!(
            plain(&npx_list(&[], &[], "~/.npm/_npx", now())),
            "npx cache (~/.npm/_npx, shared by all Node versions)\n  └─ (empty)\n"
        );
    }

    #[test]
    fn renders_npx_which_notes() {
        let cached = npx_pkg("skills", "1.7.2", "a", 60);
        assert_eq!(
            plain(&npx_found(&[&cached], false, now())),
            "not installed globally, but cached by npx:\n  skills  1.7.2  used 1 minute ago\n"
        );
        assert!(plain(&npx_found(&[&cached], true, now())).starts_with("\nalso cached by npx:"));
        assert_eq!(npx_found(&[], false, now()), "");
    }

    #[test]
    fn output_is_styled() {
        let out = tree(&sample());
        assert!(out.contains(&styled(MARK, "(default)")));
        assert!(out.contains(&styled(WARN, "(linked)")));
    }
}
