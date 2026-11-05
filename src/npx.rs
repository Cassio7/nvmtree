//! Reads npm's npx cache (`<npm cache>/_npx`): packages downloaded by `npx`.
//! They aren't installed globally and are shared by all Node versions, so
//! they're listed separately from the per-version global packages.
//!
//! Each run of `npx <specs>` gets a folder `_npx/<hash>/` with a
//! `package.json` whose `_npx.packages` lists the requested specs, and the
//! packages themselves in `node_modules/`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::SystemTime;
use std::{fs, thread};

use crate::nvm::{GlobalPackage, read_package};

/// A package `npx` downloaded, from one cache folder.
#[derive(Debug, Clone)]
pub struct NpxPackage {
    pub package: GlobalPackage,
    /// The `_npx/<hash>` folder holding it (and its dependencies).
    pub dir: PathBuf,
    /// Last time npx used the folder (its modification time).
    pub used: Option<SystemTime>,
}

/// The npx cache folder: `$npm_config_cache/_npx`, else npm's default cache
/// (`~/.npm`, or `%LOCALAPPDATA%\npm-cache` on Windows).
pub fn cache_dir() -> Option<PathBuf> {
    let env = |key| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let npm_cache = env("npm_config_cache")
        .or_else(|| env("NPM_CONFIG_CACHE"))
        .or_else(|| {
            if cfg!(windows) {
                env("LOCALAPPDATA").map(|d| d.join("npm-cache"))
            } else {
                env("HOME").map(|h| h.join(".npm"))
            }
        })?;
    Some(npm_cache.join("_npx"))
}

/// Package name from an npx spec: `skills`, `skills@1.7`, `@scope/tool@^2`.
fn spec_name(spec: &str) -> &str {
    let search_from = usize::from(spec.starts_with('@'));
    match spec[search_from..].find('@') {
        Some(at) => &spec[..search_from + at],
        None => spec,
    }
}

/// Lists the packages in the npx cache at `dir`, most recently used first.
/// A missing or unreadable cache is just empty.
pub fn packages(dir: &Path) -> Vec<NpxPackage> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let folder = entry.path();
        let Some(names) = requested_packages(&folder) else {
            continue;
        };
        let used = entry.metadata().and_then(|m| m.modified()).ok();
        for name in names {
            let package_dir = name
                .split('/')
                .fold(folder.join("node_modules"), |p, part| p.join(part));
            if package_dir.is_dir() {
                found.push(NpxPackage {
                    package: read_package(name, &package_dir),
                    dir: folder.clone(),
                    used,
                });
            }
        }
    }
    found.sort_by(|a, b| {
        b.used
            .cmp(&a.used)
            .then_with(|| a.package.name.cmp(&b.package.name))
    });
    found
}

/// Names of the packages that were asked for in an `_npx/<hash>` folder: its
/// `_npx.packages` specs, or (older npm) its `dependencies`.
fn requested_packages(folder: &Path) -> Option<Vec<String>> {
    let text = fs::read_to_string(folder.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let mut names: Vec<String> = match json.pointer("/_npx/packages").and_then(|p| p.as_array()) {
        Some(specs) => specs
            .iter()
            .filter_map(|s| s.as_str())
            .map(|s| spec_name(s).to_string())
            .collect(),
        None => json
            .get("dependencies")?
            .as_object()?
            .keys()
            .cloned()
            .collect(),
    };
    // Names become paths below: no `..`, absolute paths or empty parts.
    names.retain(|n| {
        !n.is_empty()
            && n.split('/').count() <= 2
            && n.split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
            && !n.contains('\\')
    });
    names.dedup();
    Some(names)
}

/// Cached packages whose name or one of whose commands equals `query`.
pub fn matching<'a>(packages: &'a [NpxPackage], query: &str) -> Vec<&'a NpxPackage> {
    packages
        .iter()
        .filter(|p| p.package.name == query || p.package.bins.iter().any(|b| b == query))
        .collect()
}

/// Most folders walked at once by [`dir_sizes`].
const MAX_PARALLEL_WALKS: usize = 8;

/// The [`dir_size`] of each of `dirs`, walking the folders in parallel. A
/// folder listed several times (packages from one `npx` run share it) is
/// walked once.
pub fn dir_sizes(dirs: &[&Path]) -> Vec<u64> {
    let mut unique = dirs.to_vec();
    unique.sort();
    unique.dedup();
    let sizes: Vec<AtomicU64> = unique.iter().map(|_| AtomicU64::new(0)).collect();
    let next = AtomicUsize::new(0);
    let threads = thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(MAX_PARALLEL_WALKS)
        .min(unique.len());
    thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(dir) = unique.get(i) else { break };
                    sizes[i].store(dir_size(dir), Ordering::Relaxed);
                }
            });
        }
    });
    dirs.iter()
        .map(|dir| match unique.binary_search(dir) {
            Ok(i) => sizes[i].load(Ordering::Relaxed),
            Err(_) => 0,
        })
        .collect()
}

/// Total size in bytes of the files under `dir`. Symlinks aren't followed.
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            // The type comes with the directory listing: no `stat` for
            // folders, only files need one for their length.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// An `_npx/<hash>` folder for `specs`, with `packages` = (name, package.json).
    fn add_entry(cache: &Path, hash: &str, specs: &[&str], packages: &[(&str, &str)]) -> PathBuf {
        let dir = cache.join(hash);
        let specs: Vec<String> = specs.iter().map(|s| format!("\"{s}\"")).collect();
        write(
            &dir.join("package.json"),
            &format!(r#"{{"_npx":{{"packages":[{}]}}}}"#, specs.join(",")),
        );
        for (name, json) in packages {
            write(
                &dir.join("node_modules").join(name).join("package.json"),
                json,
            );
        }
        dir
    }

    /// Sets a directory's modification time.
    fn set_mtime(path: &Path, secs_ago: u64) {
        let time = SystemTime::now() - Duration::from_secs(secs_ago);
        // Windows only opens a directory with FILE_FLAG_BACKUP_SEMANTICS, and
        // changing its times needs FILE_WRITE_ATTRIBUTES access.
        #[cfg(windows)]
        let dir = {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
            const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
            fs::OpenOptions::new()
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
                .open(path)
        };
        #[cfg(not(windows))]
        let dir = fs::File::open(path);
        dir.unwrap().set_modified(time).unwrap();
    }

    #[test]
    fn parses_spec_names() {
        assert_eq!(spec_name("skills"), "skills");
        assert_eq!(spec_name("skills@1.7.2"), "skills");
        assert_eq!(spec_name("create-vite@latest"), "create-vite");
        assert_eq!(spec_name("@scope/tool"), "@scope/tool");
        assert_eq!(spec_name("@scope/tool@^2"), "@scope/tool");
    }

    #[test]
    fn lists_cached_packages_most_recent_first() {
        let tmp = TempDir::new().unwrap();
        let cache = tmp.path();
        let old = add_entry(
            cache,
            "aaa",
            &["prettier@3"],
            &[("prettier", r#"{"version":"3.4.2","bin":{"prettier":"x"}}"#)],
        );
        let new = add_entry(
            cache,
            "bbb",
            &["skills"],
            &[
                (
                    "skills",
                    r#"{"version":"1.7.2","bin":{"skills":"x","add-skill":"x"}}"#,
                ),
                ("yaml", r#"{"version":"2.0.0"}"#), // a dependency, not listed
            ],
        );
        add_entry(
            cache,
            "ccc",
            &["@scope/tool@^2", "cowsay"],
            &[
                ("@scope/tool", r#"{"version":"2.1.0"}"#),
                ("cowsay", r#"{"version":"1.6.0"}"#),
            ],
        );
        // Not npx folders: no package.json, broken JSON, missing package.
        fs::create_dir_all(cache.join("junk")).unwrap();
        write(&cache.join("broken/package.json"), "{nope");
        add_entry(cache, "ddd", &["gone"], &[]);
        set_mtime(&old, 3600 * 24 * 90);
        set_mtime(&new, 60);
        set_mtime(&cache.join("ccc"), 3600);

        let found = packages(cache);
        let summary: Vec<_> = found
            .iter()
            .map(|p| (p.package.name.as_str(), p.package.version.as_deref()))
            .collect();
        assert_eq!(
            summary,
            [
                ("skills", Some("1.7.2")),
                ("@scope/tool", Some("2.1.0")),
                ("cowsay", Some("1.6.0")),
                ("prettier", Some("3.4.2")),
            ]
        );
        assert_eq!(found[0].package.bins, ["add-skill", "skills"]);
        assert_eq!(found[0].dir, new);
    }

    #[test]
    fn reads_legacy_entries_without_npx_field() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("old");
        write(
            &dir.join("package.json"),
            r#"{"dependencies":{"cowsay":"^1"}}"#,
        );
        write(
            &dir.join("node_modules/cowsay/package.json"),
            r#"{"version":"1.5.0"}"#,
        );
        let found = packages(tmp.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].package.name, "cowsay");
    }

    #[test]
    fn ignores_names_that_escape_the_folder() {
        let tmp = TempDir::new().unwrap();
        write(
            &tmp.path().join("secret/package.json"),
            r#"{"version":"6.6.6"}"#,
        );
        add_entry(tmp.path(), "evil", &["../../secret", "/etc", "a/b/c"], &[]);
        assert!(packages(tmp.path()).is_empty());
    }

    #[test]
    fn matches_by_name_or_command() {
        let tmp = TempDir::new().unwrap();
        add_entry(
            tmp.path(),
            "a",
            &["skills"],
            &[(
                "skills",
                r#"{"version":"1.7.2","bin":{"add-skill":"x","skills":"x"}}"#,
            )],
        );
        let cached = packages(tmp.path());
        assert_eq!(matching(&cached, "skills").len(), 1);
        assert_eq!(matching(&cached, "add-skill").len(), 1);
        assert!(matching(&cached, "skill").is_empty());
    }

    #[test]
    fn missing_cache_is_empty() {
        let tmp = TempDir::new().unwrap();
        assert!(packages(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn sums_file_sizes() {
        let tmp = TempDir::new().unwrap();
        write(&tmp.path().join("a.txt"), "12345");
        write(&tmp.path().join("sub/deeper/b.txt"), "123");
        assert_eq!(dir_size(tmp.path()), 8);
        assert_eq!(dir_size(&tmp.path().join("nope")), 0);
    }

    #[test]
    fn sizes_several_folders_in_order() {
        let tmp = TempDir::new().unwrap();
        let (a, b, missing) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("nope"),
        );
        write(&a.join("x.txt"), "12345");
        write(&b.join("sub/y.txt"), "12");
        let dirs = [b.as_path(), a.as_path(), missing.as_path(), b.as_path()];
        assert_eq!(dir_sizes(&dirs), [2, 5, 0, 2]);
        assert!(dir_sizes(&[]).is_empty());
    }
}
