//! Comparing package sets between versions and finding packages.

use std::collections::{BTreeMap, BTreeSet};

use crate::nvm::{Entry, Flavor, GlobalPackage, NodeVersion};
use crate::sanitize::{is_safe_package_name, is_safe_version};

/// Differences between the global packages of two versions.
#[derive(Debug, Default)]
pub struct Diff<'a> {
    pub only_from: Vec<&'a GlobalPackage>,
    pub only_to: Vec<&'a GlobalPackage>,
    /// Present in both with different versions: `(from, to)`.
    pub changed: Vec<(&'a GlobalPackage, &'a GlobalPackage)>,
    pub same: usize,
}

pub fn diff<'a>(from: &'a [GlobalPackage], to: &'a [GlobalPackage]) -> Diff<'a> {
    let mut d = Diff::default();
    for f in from {
        match to.iter().find(|t| t.name == f.name) {
            Some(t) if t.version == f.version => d.same += 1,
            Some(t) => d.changed.push((f, t)),
            None => d.only_from.push(f),
        }
    }
    d.only_to = to
        .iter()
        .filter(|t| !from.iter().any(|f| f.name == t.name))
        .collect();
    d
}

/// Whether `pkg` may go into a generated install command: not linked (those
/// point at local checkouts and must be re-linked by hand), a safe package
/// name and, when pinning versions, a safe version. Names and versions come
/// from folder names and `package.json`, so anything else is left out rather
/// than risk injecting shell syntax into a command the user may run.
pub fn is_installable(pkg: &GlobalPackage, exact: bool) -> bool {
    !pkg.linked
        && is_safe_package_name(&pkg.name)
        && (!exact || pkg.version.as_deref().is_none_or(is_safe_version))
}

/// Packages left out of the install command because their name or version
/// failed validation (linked packages are reported separately).
pub fn not_installable<'a>(missing: &[&'a GlobalPackage], exact: bool) -> Vec<&'a GlobalPackage> {
    missing
        .iter()
        .filter(|p| !p.linked && !is_installable(p, exact))
        .copied()
        .collect()
}

/// Shell command(s) installing `missing` into `target`, or `None` when there
/// is nothing installable (see [`is_installable`]). With `exact`, versions
/// are pinned.
pub fn install_command(
    flavor: &Flavor,
    target: &NodeVersion,
    missing: &[&GlobalPackage],
    exact: bool,
) -> Option<String> {
    let specs: Vec<String> = missing
        .iter()
        .filter(|p| is_installable(p, exact))
        .map(|p| match (&p.version, exact) {
            (Some(v), true) => format!("{}@{v}", p.name),
            _ => p.name.clone(),
        })
        .collect();
    if specs.is_empty() {
        return None;
    }
    Some(npm_install_command(flavor, target, &specs))
}

/// Shell command(s) running `npm i -g <specs>` in `target`. Callers must only
/// pass specs built from [`is_installable`] packages.
pub fn npm_install_command(flavor: &Flavor, target: &NodeVersion, specs: &[String]) -> String {
    let version = target.name.trim_start_matches('v');
    let packages = specs.join(" ");
    match flavor {
        // `nvm exec` runs in the target version without switching the shell.
        Flavor::NvmSh => format!("nvm exec {version} npm i -g {packages}"),
        // nvm-windows has no `exec`; two lines work in both cmd and PowerShell.
        Flavor::NvmWindows { .. } => format!("nvm use {version}\nnpm i -g {packages}"),
    }
}

/// A package found by [`find`], with where it is and isn't installed.
#[derive(Debug)]
pub struct Found<'a> {
    pub name: &'a str,
    /// Union of commands it provides across versions, sorted.
    pub bins: Vec<&'a str>,
    pub installed: Vec<(&'a Entry, &'a GlobalPackage)>,
    pub missing: Vec<&'a Entry>,
}

/// Packages whose name or one of whose commands equals `query`. Once a
/// package matches in any version, every version that has it is reported,
/// even where that version's copy doesn't provide the queried command.
pub fn find<'a>(entries: &'a [Entry], query: &str) -> Vec<Found<'a>> {
    let all_packages = || {
        entries
            .iter()
            .flat_map(|e| e.packages.iter().map(move |p| (e, p)))
    };
    let matching: BTreeSet<&str> = all_packages()
        .filter(|(_, p)| p.name == query || p.bins.iter().any(|b| b == query))
        .map(|(_, p)| p.name.as_str())
        .collect();

    let mut by_name: BTreeMap<&str, Vec<(&Entry, &GlobalPackage)>> = BTreeMap::new();
    for (entry, pkg) in all_packages() {
        if matching.contains(pkg.name.as_str()) {
            by_name.entry(&pkg.name).or_default().push((entry, pkg));
        }
    }
    by_name
        .into_iter()
        .map(|(name, installed)| {
            let mut bins: Vec<&str> = installed
                .iter()
                .flat_map(|(_, p)| p.bins.iter().map(String::as_str))
                .collect();
            bins.sort();
            bins.dedup();
            let missing = entries
                .iter()
                .filter(|e| !installed.iter().any(|(ie, _)| ie.version == e.version))
                .collect();
            Found {
                name,
                bins,
                installed,
                missing,
            }
        })
        .collect()
}

/// Package and command names containing `query` (case-insensitive), for
/// "did you mean" hints.
pub fn suggestions<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a str> {
    let query = query.to_lowercase();
    let mut names: Vec<&str> = entries
        .iter()
        .flat_map(|e| &e.packages)
        .flat_map(|p| std::iter::once(p.name.as_str()).chain(p.bins.iter().map(String::as_str)))
        .filter(|n| n.to_lowercase().contains(&query))
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn pkg(name: &str, version: &str, bins: &[&str], linked: bool) -> GlobalPackage {
        GlobalPackage {
            name: name.to_string(),
            version: Some(version.to_string()),
            linked,
            bins: bins.iter().map(|b| b.to_string()).collect(),
            ..Default::default()
        }
    }

    fn version(name: &str) -> NodeVersion {
        NodeVersion {
            name: name.to_string(),
            parts: crate::nvm::parse_version(name).unwrap(),
            path: PathBuf::new(),
        }
    }

    fn entry(name: &str, packages: Vec<GlobalPackage>) -> Entry {
        Entry {
            version: version(name),
            label: None,
            packages,
            node: None,
        }
    }

    fn names<'a>(packages: &[&'a GlobalPackage]) -> Vec<&'a str> {
        packages.iter().map(|p| p.name.as_str()).collect()
    }

    #[test]
    fn diffs_package_sets() {
        let from = [
            pkg("@shopify/cli", "4.8.4", &["shopify"], false),
            pkg("npm", "11.19.0", &[], false),
            pkg("pnpm", "10.0.0", &[], false),
            pkg("yoinks", "0.3.1", &[], false),
        ];
        let to = [
            pkg("npm", "11.20.0", &[], false),
            pkg("pnpm", "10.0.0", &[], false),
            pkg("typescript", "7.0.2", &["tsc"], false),
        ];
        let d = diff(&from, &to);
        assert_eq!(names(&d.only_from), ["@shopify/cli", "yoinks"]);
        assert_eq!(names(&d.only_to), ["typescript"]);
        assert_eq!(d.changed.len(), 1);
        assert_eq!(d.changed[0].0.name, "npm");
        assert_eq!(d.same, 1);
    }

    #[test]
    fn builds_install_commands() {
        let a = pkg("@shopify/cli", "4.8.4", &[], false);
        let b = pkg("yoinks", "0.3.1", &[], false);
        let linked = pkg("mytool", "1.0.0", &[], true);
        let missing = [&a, &linked, &b];
        let target = version("v26.11.1");

        assert_eq!(
            install_command(&Flavor::NvmSh, &target, &missing, false).unwrap(),
            "nvm exec 26.11.1 npm i -g @shopify/cli yoinks"
        );
        assert_eq!(
            install_command(&Flavor::NvmSh, &target, &missing, true).unwrap(),
            "nvm exec 26.11.1 npm i -g @shopify/cli@4.8.4 yoinks@0.3.1"
        );
        assert_eq!(
            install_command(
                &Flavor::NvmWindows { symlink: None },
                &target,
                &missing,
                false
            )
            .unwrap(),
            "nvm use 26.11.1\nnpm i -g @shopify/cli yoinks"
        );
        assert_eq!(
            install_command(&Flavor::NvmSh, &target, &[&linked], false),
            None
        );
    }

    #[test]
    fn install_commands_never_contain_unsafe_names_or_versions() {
        let good = pkg("@shopify/cli", "4.8.4", &[], false);
        let evil_name = pkg("evil;name", "1.0.0", &[], false);
        let evil_version = pkg("yoinks", "1.0.0 && echo INJECTED", &[], false);
        let linked = pkg("mytool", "1.0.0", &[], true);
        let missing = [&good, &evil_name, &evil_version, &linked];
        let target = version("v26.11.1");

        // Without pinning, only the name matters: yoinks is fine.
        assert_eq!(
            install_command(&Flavor::NvmSh, &target, &missing, false).unwrap(),
            "nvm exec 26.11.1 npm i -g @shopify/cli yoinks"
        );
        assert_eq!(names(&not_installable(&missing, false)), ["evil;name"]);

        // With --exact, the tampered version is left out too.
        assert_eq!(
            install_command(&Flavor::NvmSh, &target, &missing, true).unwrap(),
            "nvm exec 26.11.1 npm i -g @shopify/cli@4.8.4"
        );
        assert_eq!(
            names(&not_installable(&missing, true)),
            ["evil;name", "yoinks"]
        );

        assert_eq!(
            install_command(&Flavor::NvmSh, &target, &[&evil_name], false),
            None
        );
    }

    #[test]
    fn finds_by_name_or_command() {
        let entries = [
            entry("v26.0.0", vec![pkg("typescript", "7.0.2", &["tsc"], false)]),
            entry(
                "v24.0.0",
                vec![pkg("typescript", "5.9.0", &["tsc", "tsserver"], false)],
            ),
            entry("v22.0.0", vec![pkg("yoinks", "0.3.1", &[], false)]),
        ];

        for query in ["typescript", "tsc", "tsserver"] {
            let found = find(&entries, query);
            assert_eq!(found.len(), 1, "{query}");
            let f = &found[0];
            assert_eq!(f.name, "typescript");
            assert_eq!(f.bins, ["tsc", "tsserver"]);
            assert_eq!(f.installed.len(), 2);
            assert_eq!(f.missing.len(), 1);
            assert_eq!(f.missing[0].version.name, "v22.0.0");
        }

        assert!(find(&entries, "type").is_empty());
        assert_eq!(suggestions(&entries, "TS"), ["tsc", "tsserver"]);
        assert_eq!(suggestions(&entries, "script"), ["typescript"]);
    }
}
