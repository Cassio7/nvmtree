//! Reads an nvm installation straight from the filesystem.
//!
//! Two layouts are supported:
//! - [nvm-sh](https://github.com/nvm-sh/nvm) on Linux and macOS
//! - [nvm-windows](https://github.com/coreybutler/nvm-windows) on Windows

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::sanitize::printable;

/// Packages that ship with Node itself rather than being installed by the user.
pub const BUNDLED_PACKAGES: &[&str] = &["npm", "corepack"];

/// An installed Node version, e.g. `v24.21.0`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeVersion {
    /// Directory name as nvm stores it, including the leading `v`.
    pub name: String,
    pub parts: (u64, u64, u64),
    pub path: PathBuf,
}

/// A package found in a version's global `node_modules`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalPackage {
    /// Full name, including the scope for scoped packages (`@scope/name`).
    pub name: String,
    /// Version from `package.json`, if it could be read.
    pub version: Option<String>,
    /// True when installed via `npm link` (the entry is a symlink).
    pub linked: bool,
    /// Commands the package puts on `PATH` (its `bin` field), sorted.
    pub bins: Vec<String>,
    /// Update / deprecation info from the npm registry, when looked up.
    pub registry: crate::registry::Info,
}

/// One Node version with its global packages.
#[derive(Debug, Clone)]
pub struct Entry {
    pub version: NodeVersion,
    /// `Some("default")` / `Some("current")` for the version nvm will use.
    pub label: Option<&'static str>,
    pub packages: Vec<GlobalPackage>,
    /// Node update / superseded info, when checked (`-u`).
    pub node: Option<crate::node::Status>,
}

/// Which nvm implementation the installation belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Flavor {
    /// nvm-sh: versions in `<root>/versions/node/vX.Y.Z`, packages in `lib/node_modules`.
    NvmSh,
    /// nvm-windows: versions in `<root>/vX.Y.Z`, packages in `node_modules`.
    /// The active version is the target of the `symlink` directory.
    NvmWindows { symlink: Option<PathBuf> },
}

/// A located nvm installation.
#[derive(Debug, Clone)]
pub struct Install {
    pub flavor: Flavor,
    /// Directory containing the installed versions' parent (nvm-sh) or the
    /// versions themselves (nvm-windows).
    pub root: PathBuf,
    /// Where nvm downloads Node from, when not nodejs.org:
    /// `$NVM_NODEJS_ORG_MIRROR` (nvm-sh), `node_mirror:` in `settings.txt`
    /// (nvm-windows).
    pub node_mirror: Option<String>,
}

impl Install {
    /// Locates the nvm installation for the current platform from the environment.
    pub fn detect() -> Result<Self> {
        if cfg!(windows) {
            let home = match env_path("NVM_HOME") {
                Some(home) => home,
                None => env_path("APPDATA")
                    .context("neither NVM_HOME nor APPDATA is set")?
                    .join("nvm"),
            };
            Ok(Self::nvm_windows(&home, env_path("NVM_SYMLINK")))
        } else {
            let root = match env_path("NVM_DIR") {
                Some(dir) => dir,
                None => env_path("HOME")
                    .context("neither NVM_DIR nor HOME is set")?
                    .join(".nvm"),
            };
            Ok(Self {
                node_mirror: std::env::var("NVM_NODEJS_ORG_MIRROR")
                    .ok()
                    .filter(|v| !v.trim().is_empty()),
                ..Self::nvm_sh(root)
            })
        }
    }

    pub fn nvm_sh(root: PathBuf) -> Self {
        Self {
            flavor: Flavor::NvmSh,
            root,
            node_mirror: None,
        }
    }

    /// Builds an nvm-windows install from its home directory (`NVM_HOME`).
    /// `settings.txt` there may override where versions live (`root:`) and
    /// the active-version symlink (`path:`); `symlink_env` wins over the latter.
    pub fn nvm_windows(home: &Path, symlink_env: Option<PathBuf>) -> Self {
        let settings = fs::read_to_string(home.join("settings.txt")).unwrap_or_default();
        let setting = |key: &str| {
            settings.lines().find_map(|line| {
                let (k, v) = line.split_once(':')?;
                let v = v.trim();
                (k.trim().eq_ignore_ascii_case(key) && !v.is_empty()).then(|| v.to_string())
            })
        };
        Self {
            root: setting("root").map_or_else(|| home.to_path_buf(), PathBuf::from),
            flavor: Flavor::NvmWindows {
                symlink: symlink_env.or_else(|| setting("path").map(PathBuf::from)),
            },
            node_mirror: setting("node_mirror"),
        }
    }

    fn versions_dir(&self) -> PathBuf {
        match self.flavor {
            Flavor::NvmSh => self.root.join("versions").join("node"),
            Flavor::NvmWindows { .. } => self.root.clone(),
        }
    }

    fn modules_dir(&self, version: &NodeVersion) -> PathBuf {
        match self.flavor {
            Flavor::NvmSh => version.path.join("lib").join("node_modules"),
            Flavor::NvmWindows { .. } => version.path.join("node_modules"),
        }
    }

    /// Lists installed Node versions, newest first.
    /// Explains that nvm isn't installed (or isn't where we looked) and how to fix it.
    fn not_found_message(&self, dir: &Path) -> String {
        let (name, url, env) = match self.flavor {
            Flavor::NvmSh => ("nvm", "https://github.com/nvm-sh/nvm", "NVM_DIR"),
            Flavor::NvmWindows { .. } => (
                "nvm-windows",
                "https://github.com/coreybutler/nvm-windows",
                "NVM_HOME",
            ),
        };
        format!(
            "{name} not found (looked in {}).\n\
             nvmtree lists the Node versions installed with {name}: {url}\n\
             If {name} is installed somewhere else, set {env} to its folder.",
            dir.display()
        )
    }

    pub fn installed_versions(&self) -> Result<Vec<NodeVersion>> {
        let dir = self.versions_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                bail!("{}", self.not_found_message(&dir))
            }
            Err(e) => {
                return Err(e).with_context(|| format!("cannot read {}", dir.display()));
            }
        };

        let mut versions = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(parts) = parse_version(&name)
                && entry.path().is_dir()
            {
                versions.push(NodeVersion {
                    name,
                    parts,
                    path: entry.path(),
                });
            }
        }
        versions.sort_by_key(|v| std::cmp::Reverse(v.parts));
        Ok(versions)
    }

    /// The version nvm will use by default, with a label describing why:
    /// the `default` alias on nvm-sh, the active symlink on nvm-windows.
    /// `installed` must be sorted newest first.
    pub fn marked_version<'a>(
        &self,
        installed: &'a [NodeVersion],
    ) -> Option<(&'a NodeVersion, &'static str)> {
        match &self.flavor {
            Flavor::NvmSh => {
                resolve_alias(&self.root, "default", installed).map(|v| (v, "default"))
            }
            Flavor::NvmWindows { symlink } => {
                let target = fs::canonicalize(symlink.as_ref()?).ok()?;
                installed
                    .iter()
                    .find(|v| fs::canonicalize(&v.path).is_ok_and(|p| p == target))
                    .map(|v| (v, "current"))
            }
        }
    }

    /// Lists the global packages of a Node version, sorted by name.
    pub fn global_packages(&self, version: &NodeVersion) -> Result<Vec<GlobalPackage>> {
        let dir = self.modules_dir(version);
        let mut packages = Vec::new();
        // A version with no global modules directory just has no packages.
        let Ok(entries) = fs::read_dir(&dir) else {
            return Ok(packages);
        };

        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            if name.starts_with('@') {
                for scoped in fs::read_dir(entry.path())? {
                    let scoped = scoped?;
                    let sub = scoped.file_name().to_string_lossy().into_owned();
                    if !sub.starts_with('.') {
                        packages.push(read_package(format!("{name}/{sub}"), &scoped.path()));
                    }
                }
            } else {
                packages.push(read_package(name, &entry.path()));
            }
        }
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(packages)
    }

    /// Loads every installed version (newest first) with its global packages.
    /// Packages bundled with Node are dropped unless `include_bundled`.
    pub fn load_entries(&self, include_bundled: bool) -> Result<Vec<Entry>> {
        let versions = self.installed_versions()?;
        let marked = self
            .marked_version(&versions)
            .map(|(v, label)| (v.name.clone(), label));
        let mut entries = Vec::with_capacity(versions.len());
        for version in versions {
            let packages = self
                .global_packages(&version)?
                .into_iter()
                .filter(|p| include_bundled || !BUNDLED_PACKAGES.contains(&p.name.as_str()))
                .collect();
            let label = marked
                .as_ref()
                .filter(|(name, _)| *name == version.name)
                .map(|(_, label)| *label);
            entries.push(Entry {
                version,
                label,
                packages,
                node: None,
            });
        }
        Ok(entries)
    }

    /// Indices of the entries matching a version spec, newest first.
    ///
    /// - `default` / `current`: the marked version
    /// - `node` / `stable`: the newest version
    /// - an nvm-sh alias name (`lts/*`, `lts/jod`, custom aliases): what it resolves to
    /// - a numeric prefix (`24`, `22.9`, `v24.21.0`): every matching version
    pub fn select(&self, entries: &[Entry], spec: &str) -> Vec<usize> {
        let spec = spec.trim();
        if spec == "default" || spec == "current" {
            return entries
                .iter()
                .position(|e| e.label.is_some())
                .into_iter()
                .collect();
        }

        let versions: Vec<NodeVersion> = entries.iter().map(|e| e.version.clone()).collect();
        let is_alias =
            matches!(self.flavor, Flavor::NvmSh) && self.root.join("alias").join(spec).is_file();
        if is_alias || spec == "node" || spec == "stable" {
            let resolved = match self.flavor {
                Flavor::NvmSh => resolve_alias(&self.root, spec, &versions),
                Flavor::NvmWindows { .. } => versions.first(),
            };
            return resolved
                .and_then(|r| versions.iter().position(|v| v == r))
                .into_iter()
                .collect();
        }

        let Some(wanted) = version_prefix(spec) else {
            return Vec::new();
        };
        (0..entries.len())
            .filter(|&i| matches_prefix(&versions[i], &wanted))
            .collect()
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Parses `v1.2.3` (or `1.2.3`) into its numeric parts.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let mut it = s.strip_prefix('v').unwrap_or(s).split('.');
    let parts = (
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
    );
    it.next().is_none().then_some(parts)
}

/// Resolves an nvm-sh version spec (`24`, `v24.21.0`, `node`, `lts/*`, an
/// alias name, ...) to the newest installed version that matches it.
/// `installed` must be sorted newest first.
pub fn resolve_alias<'a>(
    nvm_dir: &Path,
    spec: &str,
    installed: &'a [NodeVersion],
) -> Option<&'a NodeVersion> {
    let alias_dir = nvm_dir.join("alias");
    let mut spec = spec.trim().to_string();

    // Aliases can point at other aliases (e.g. lts/* -> lts/jod -> v22.x).
    for _ in 0..10 {
        match fs::read_to_string(alias_dir.join(&spec)) {
            Ok(target) => spec = target.trim().to_string(),
            Err(_) => break,
        }
    }

    match spec.as_str() {
        "node" | "stable" => return installed.first(),
        _ => {}
    }

    let wanted = version_prefix(&spec)?;
    installed.iter().find(|v| matches_prefix(v, &wanted))
}

/// Parses a (possibly partial) version like `24`, `v22.9` or `24.21.0`.
fn version_prefix(spec: &str) -> Option<Vec<u64>> {
    let parts: Vec<u64> = spec
        .strip_prefix('v')
        .unwrap_or(spec)
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    (parts.len() <= 3).then_some(parts)
}

fn matches_prefix(version: &NodeVersion, wanted: &[u64]) -> bool {
    [version.parts.0, version.parts.1, version.parts.2].starts_with(wanted)
}

pub(crate) fn read_package(name: String, path: &Path) -> GlobalPackage {
    let linked = fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    let json = fs::read_to_string(path.join("package.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok());
    let version = json
        .as_ref()
        .and_then(|j| j.get("version")?.as_str())
        .map(|v| printable(v).into_owned());
    let bins = json
        .as_ref()
        .and_then(|j| j.get("bin"))
        .map(|bin| bin_names(&name, bin))
        .unwrap_or_default();
    GlobalPackage {
        name,
        version,
        linked,
        bins,
        registry: Default::default(),
    }
}

/// Command names from a `package.json` `bin` field. A plain string means a
/// single command named after the package (without its scope).
fn bin_names(package: &str, bin: &serde_json::Value) -> Vec<String> {
    let mut names: Vec<String> = match bin {
        serde_json::Value::String(_) => {
            let unscoped = package.rsplit('/').next().unwrap_or(package);
            vec![printable(unscoped).into_owned()]
        }
        serde_json::Value::Object(map) => map.keys().map(|k| printable(k).into_owned()).collect(),
        _ => Vec::new(),
    };
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn add_packages(modules: &Path) {
        write(
            &modules.join("npm/package.json"),
            r#"{"version":"11.0.0","bin":{"npx":"bin/npx-cli.js","npm":"bin/npm-cli.js"}}"#,
        );
        write(
            &modules.join("yoinks/package.json"),
            r#"{"version":"1.2.3"}"#,
        );
        write(
            &modules.join("@shopify/cli/package.json"),
            r#"{"version":"3.80.0","bin":"bin/run.js"}"#,
        );
        fs::create_dir_all(modules.join("broken")).unwrap();
        fs::create_dir_all(modules.join(".bin")).unwrap();
    }

    const VERSIONS: [&str; 4] = ["v18.20.4", "v22.11.0", "v22.9.0", "v24.21.0"];

    fn fake_nvm_sh() -> (TempDir, Install) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        for v in VERSIONS {
            fs::create_dir_all(root.join("versions/node").join(v)).unwrap();
        }
        add_packages(&root.join("versions/node/v24.21.0/lib/node_modules"));
        write(&root.join("alias/default"), "22\n");
        // `lts/*` can't be a file name on Windows, so chain through another alias.
        write(&root.join("alias/work"), "lts/jod\n");
        write(&root.join("alias/lts/jod"), "v22.11.0\n");
        let install = Install::nvm_sh(root.to_path_buf());
        (tmp, install)
    }

    fn fake_nvm_windows(symlink_env: Option<&str>) -> (TempDir, Install) {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("nvm");
        for v in VERSIONS {
            fs::create_dir_all(home.join(v)).unwrap();
        }
        add_packages(&home.join("v24.21.0/node_modules"));
        write(&home.join("nvm.exe"), "");
        write(
            &home.join("settings.txt"),
            &format!("root: {}\npath: \n", home.display()),
        );
        let install = Install::nvm_windows(&home, symlink_env.map(|v| home.join(v)));
        (tmp, install)
    }

    fn names(versions: &[NodeVersion]) -> Vec<&str> {
        versions.iter().map(|v| v.name.as_str()).collect()
    }

    fn package_summary(packages: &[GlobalPackage]) -> Vec<(&str, Option<&str>)> {
        packages
            .iter()
            .map(|p| (p.name.as_str(), p.version.as_deref()))
            .collect()
    }

    const EXPECTED_PACKAGES: [(&str, Option<&str>); 4] = [
        ("@shopify/cli", Some("3.80.0")),
        ("broken", None),
        ("npm", Some("11.0.0")),
        ("yoinks", Some("1.2.3")),
    ];

    #[test]
    fn missing_nvm_gives_helpful_error() {
        let tmp = TempDir::new().unwrap();
        let err = Install::nvm_sh(tmp.path().join("nope"))
            .installed_versions()
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("nvm not found (looked in "), "{err}");
        assert!(err.contains("https://github.com/nvm-sh/nvm"), "{err}");
        assert!(err.contains("set NVM_DIR"), "{err}");

        let err = Install::nvm_windows(&tmp.path().join("nope"), None)
            .installed_versions()
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("nvm-windows not found"), "{err}");
        assert!(err.contains("set NVM_HOME"), "{err}");
    }

    #[test]
    fn parses_versions() {
        assert_eq!(parse_version("v24.21.0"), Some((24, 21, 0)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v24"), None);
        assert_eq!(parse_version("v1.2.3.4"), None);
        assert_eq!(parse_version("system"), None);
    }

    #[test]
    fn nvm_sh_lists_versions_newest_first() {
        let (_tmp, install) = fake_nvm_sh();
        let installed = install.installed_versions().unwrap();
        assert_eq!(
            names(&installed),
            ["v24.21.0", "v22.11.0", "v22.9.0", "v18.20.4"]
        );
    }

    #[test]
    fn nvm_sh_resolves_aliases() {
        let (_tmp, install) = fake_nvm_sh();
        let installed = install.installed_versions().unwrap();
        let resolve = |s| resolve_alias(&install.root, s, &installed).map(|v| v.name.as_str());

        assert_eq!(resolve("default"), Some("v22.11.0"));
        assert_eq!(resolve("work"), Some("v22.11.0"));
        assert_eq!(resolve("node"), Some("v24.21.0"));
        assert_eq!(resolve("22.9"), Some("v22.9.0"));
        assert_eq!(resolve("v18.20.4"), Some("v18.20.4"));
        assert_eq!(resolve("20"), None);
        assert_eq!(resolve("nonsense"), None);

        let (marked, label) = install.marked_version(&installed).unwrap();
        assert_eq!((marked.name.as_str(), label), ("v22.11.0", "default"));
    }

    #[test]
    fn selects_versions_by_spec() {
        let (_tmp, install) = fake_nvm_sh();
        let entries = install.load_entries(false).unwrap();
        let select = |spec| -> Vec<&str> {
            install
                .select(&entries, spec)
                .into_iter()
                .map(|i| entries[i].version.name.as_str())
                .collect()
        };

        assert_eq!(select("22"), ["v22.11.0", "v22.9.0"]);
        assert_eq!(select("v22.9"), ["v22.9.0"]);
        assert_eq!(select("24.21.0"), ["v24.21.0"]);
        assert_eq!(select("default"), ["v22.11.0"]);
        assert_eq!(select("work"), ["v22.11.0"]);
        assert_eq!(select("node"), ["v24.21.0"]);
        assert!(select("20").is_empty());
        assert!(select("nonsense").is_empty());
        assert!(select("1.2.3.4").is_empty());
    }

    #[test]
    fn load_entries_filters_bundled_and_labels_default() {
        let (_tmp, install) = fake_nvm_sh();
        let entries = install.load_entries(false).unwrap();
        let newest: Vec<_> = entries[0]
            .packages
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(newest, ["@shopify/cli", "broken", "yoinks"]);
        let labels: Vec<_> = entries.iter().map(|e| e.label).collect();
        assert_eq!(labels, [None, Some("default"), None, None]);
        assert_eq!(install.load_entries(true).unwrap()[0].packages.len(), 4);
    }

    #[test]
    fn nvm_sh_reads_global_packages() {
        let (_tmp, install) = fake_nvm_sh();
        let installed = install.installed_versions().unwrap();
        let packages = install.global_packages(&installed[0]).unwrap();
        assert_eq!(package_summary(&packages), EXPECTED_PACKAGES);
        // Versions without lib/node_modules have no packages.
        assert!(install.global_packages(&installed[3]).unwrap().is_empty());
    }

    #[test]
    fn reads_package_bins() {
        let (_tmp, install) = fake_nvm_sh();
        let installed = install.installed_versions().unwrap();
        let packages = install.global_packages(&installed[0]).unwrap();
        let bins: Vec<_> = packages
            .iter()
            .map(|p| (p.name.as_str(), p.bins.clone()))
            .collect();
        assert_eq!(
            bins,
            [
                ("@shopify/cli", vec!["cli".to_string()]),
                ("broken", vec![]),
                ("npm", vec!["npm".to_string(), "npx".to_string()]),
                ("yoinks", vec![]),
            ]
        );
    }

    #[test]
    fn nvm_windows_lists_versions_and_packages() {
        let (_tmp, install) = fake_nvm_windows(None);
        let installed = install.installed_versions().unwrap();
        // nvm.exe and settings.txt are ignored.
        assert_eq!(
            names(&installed),
            ["v24.21.0", "v22.11.0", "v22.9.0", "v18.20.4"]
        );
        let packages = install.global_packages(&installed[0]).unwrap();
        assert_eq!(package_summary(&packages), EXPECTED_PACKAGES);
        // Empty `path:` in settings and no env var: nothing to mark.
        assert!(install.marked_version(&installed).is_none());
    }

    #[test]
    fn nvm_windows_settings_root_overrides_home() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().join("nvm");
        let versions = tmp.path().join("elsewhere");
        fs::create_dir_all(versions.join("v20.1.0")).unwrap();
        write(
            &home.join("settings.txt"),
            &format!(
                "root: {}\nnode_mirror: https://npmmirror.com/mirrors/node/\n",
                versions.display()
            ),
        );
        let install = Install::nvm_windows(&home, None);
        assert_eq!(names(&install.installed_versions().unwrap()), ["v20.1.0"]);
        assert_eq!(
            install.node_mirror.as_deref(),
            Some("https://npmmirror.com/mirrors/node/")
        );
    }

    #[test]
    fn nvm_windows_marks_symlinked_version() {
        // Pointing the "symlink" straight at a version dir exercises the
        // comparison on every platform without needing symlink privileges.
        let (_tmp, install) = fake_nvm_windows(Some("v22.9.0"));
        let installed = install.installed_versions().unwrap();
        let (marked, label) = install.marked_version(&installed).unwrap();
        assert_eq!((marked.name.as_str(), label), ("v22.9.0", "current"));
    }

    #[cfg(unix)]
    #[test]
    fn nvm_windows_follows_real_symlink() {
        let (tmp, _) = fake_nvm_windows(None);
        let home = tmp.path().join("nvm");
        let link = tmp.path().join("nodejs");
        std::os::unix::fs::symlink(home.join("v18.20.4"), &link).unwrap();
        let install = Install::nvm_windows(&home, Some(link));
        let installed = install.installed_versions().unwrap();
        let (marked, _) = install.marked_version(&installed).unwrap();
        assert_eq!(marked.name, "v18.20.4");
    }
}
