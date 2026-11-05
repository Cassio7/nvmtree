//! Looks up the latest version and deprecation notices of packages on the npm
//! registry. Answers are cached on disk so the default command stays fast:
//! most runs make no network requests at all.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, thread};

use serde::{Deserialize, Serialize};

use crate::sanitize::{is_safe_package_name, is_safe_version};

/// How long a registry answer is reused before asking again.
pub const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
/// Version lists are much bigger than single manifests (~2 MB for
/// `@angular/cli`), and only fetched when `latest` needs a newer Node.
const VERSIONS_TIMEOUT: Duration = Duration::from_secs(10);
const VERSIONS_BODY_LIMIT: u64 = 64 * 1024 * 1024;
/// Bumped when cached manifests gain fields, so older caches are refetched.
const CACHE_FORMAT: u32 = 2;

type NodeVersion = (u64, u64, u64);
const MAX_PARALLEL_REQUESTS: usize = 16;

/// The parts of a package manifest we care about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: Option<String>,
    pub deprecated: Option<String>,
    /// `engines.node` of this version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engines_node: Option<String>,
    /// Only for version-list keys (the bare package name): every stable,
    /// non-deprecated version with its `engines.node`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<Published>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engines_node: Option<String>,
}

/// Result of asking the registry for one manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    Found(Manifest),
    /// The registry doesn't know it (e.g. a private package).
    Missing,
    /// Network error, timeout or unexpected response.
    Failed,
}

/// What the registry says about one installed package version.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    /// The newest version newer than the installed one that runs on its Node
    /// version: `latest`, or an older one when `latest` needs a newer Node.
    pub update: Option<String>,
    /// Set when `latest` is newer but doesn't support this Node version.
    pub latest_needs_node: Option<NeedsNode>,
    /// Deprecation message of the installed version.
    pub deprecated: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeedsNode {
    /// The `latest` version.
    pub latest: String,
    /// Its `engines.node` range.
    pub engines_node: String,
    /// The oldest Node newer than the current one that satisfies the range.
    pub min_node: Option<NodeVersion>,
}

impl Info {
    /// Whether `update` is the registry's `latest` (so `@latest` installs it).
    pub fn update_is_latest(&self) -> bool {
        self.latest_needs_node.is_none()
    }
}

/// One installed package to look up: name, version, and the Node version it
/// runs on (`None` when unknown, e.g. npx: no engine check).
pub type Query = (String, String, Option<NodeVersion>);

#[derive(Debug, Default)]
pub struct Lookup {
    /// Keyed by the [`Query`].
    pub info: HashMap<Query, Info>,
    /// True when some answers are missing because the registry couldn't be
    /// reached and nothing was cached.
    pub incomplete: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    format: u32,
    entries: HashMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    /// `None` when the registry answered 404.
    manifest: Option<Manifest>,
    fetched_at: u64,
}

/// Registry path for a manifest: `name/latest` or `name/1.2.3`, with the
/// scope separator escaped as the registry expects (`@scope%2Fname/latest`).
fn manifest_key(name: &str, version: &str) -> String {
    format!("{}/{version}", versions_key(name))
}

/// Registry path for a package's version list (`@scope%2Fname`). Has no `/`,
/// which is how the fetcher tells it from a manifest key.
fn versions_key(name: &str) -> String {
    name.replace('/', "%2F")
}

/// Looks up installed packages, using the cache at `cache_path` for answers
/// younger than `ttl` (all of them are refetched when `refresh`). `fetch` is
/// called in parallel for everything else.
pub fn lookup(
    packages: &[Query],
    cache_path: Option<&Path>,
    ttl: Duration,
    refresh: bool,
    fetch: impl Fn(&str) -> Fetched + Sync,
) -> Lookup {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut cache: Cache = cache_path
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|c: &Cache| c.format == CACHE_FORMAT)
        .unwrap_or_default();
    cache.format = CACHE_FORMAT;

    // Names and versions end up in request URLs; skip anything that isn't a
    // plain package name / semver version (they get no info).
    let packages: Vec<&Query> = packages
        .iter()
        .filter(|(name, version, _)| is_safe_package_name(name) && is_safe_version(version))
        .collect();

    let mut keys: Vec<String> = Vec::new();
    for (name, version, _) in &packages {
        keys.extend([manifest_key(name, "latest"), manifest_key(name, version)]);
    }
    let mut failed = HashSet::new();
    let mut changed = update_cache(&mut cache, keys, now, ttl, refresh, &fetch, &mut failed);

    // Second round, only for packages whose `latest` doesn't run on the Node
    // version they're installed for: their version lists.
    let latest = |cache: &Cache, name: &str| -> Option<Manifest> {
        cache
            .entries
            .get(&manifest_key(name, "latest"))
            .and_then(|e| e.manifest.clone())
    };
    let needs_node = |latest: &Manifest, version: &str, node: Option<NodeVersion>| {
        let to = latest.version.as_deref().filter(|l| is_newer(l, version))?;
        let range = latest.engines_node.as_deref()?;
        let node = node?;
        (crate::engines::satisfies(range, node) == Some(false)).then(|| NeedsNode {
            latest: to.to_string(),
            engines_node: range.to_string(),
            min_node: crate::engines::min_newer(range, node),
        })
    };
    let lists: Vec<String> = packages
        .iter()
        .filter(|(name, version, node)| {
            latest(&cache, name).is_some_and(|l| needs_node(&l, version, *node).is_some())
        })
        .map(|(name, _, _)| versions_key(name))
        .collect();
    changed |= update_cache(&mut cache, lists, now, ttl, refresh, &fetch, &mut failed);

    if changed && let Some(path) = cache_path {
        save_cache(path, &cache);
    }

    let manifest = |key: &str| cache.entries.get(key).and_then(|e| e.manifest.as_ref());
    let mut result = Lookup {
        incomplete: !failed.is_empty(),
        ..Lookup::default()
    };
    for query in packages {
        let (name, version, node) = query;
        let latest = manifest(&manifest_key(name, "latest"));
        let installed = manifest(&manifest_key(name, version));
        let latest_needs_node = latest.and_then(|l| needs_node(l, version, *node));
        let update = match (&latest_needs_node, node) {
            (None, _) => latest
                .and_then(|m| m.version.as_deref())
                .filter(|l| is_newer(l, version))
                .map(str::to_string),
            (Some(needs), Some(node)) => manifest(&versions_key(name))
                .and_then(|list| newest_for_node(&list.versions, version, &needs.latest, *node)),
            (Some(_), None) => None,
        };
        let info = Info {
            update,
            latest_needs_node,
            deprecated: installed.and_then(|m| m.deprecated.clone()),
        };
        result.info.insert(query.clone(), info);
    }
    result
}

/// The newest of `versions` newer than `installed`, not newer than `latest`,
/// whose `engines.node` accepts `node` (a missing or unreadable range counts
/// as accepting it).
fn newest_for_node(
    versions: &[Published],
    installed: &str,
    latest: &str,
    node: NodeVersion,
) -> Option<String> {
    versions
        .iter()
        .filter(|p| is_newer(&p.version, installed) && !is_newer(&p.version, latest))
        .filter(|p| {
            p.engines_node
                .as_deref()
                .and_then(|range| crate::engines::satisfies(range, node))
                .unwrap_or(true)
        })
        .filter_map(|p| Some((crate::nvm::parse_version(&p.version)?, &p.version)))
        .max()
        .map(|(_, v)| v.clone())
}

/// Fetches the `keys` that aren't freshly cached and stores the answers.
/// Keys that failed with nothing cached go into `failed`. Returns whether the
/// cache changed.
fn update_cache(
    cache: &mut Cache,
    keys: Vec<String>,
    now: u64,
    ttl: Duration,
    refresh: bool,
    fetch: &(impl Fn(&str) -> Fetched + Sync),
    failed: &mut HashSet<String>,
) -> bool {
    let is_fresh =
        |entry: &CacheEntry| !refresh && now.saturating_sub(entry.fetched_at) < ttl.as_secs();
    let mut seen = HashSet::new();
    let stale: Vec<String> = keys
        .into_iter()
        .filter(|k| seen.insert(k.clone()))
        .filter(|k| !cache.entries.get(k).is_some_and(is_fresh))
        .collect();

    let mut changed = false;
    for (key, result) in fetch_all(&stale, fetch) {
        let manifest = match result {
            Fetched::Found(m) => Some(m),
            Fetched::Missing => None,
            // Keep any older cached answer: better slightly stale than nothing.
            Fetched::Failed => {
                if !cache.entries.contains_key(&key) {
                    failed.insert(key);
                }
                continue;
            }
        };
        cache.entries.insert(
            key,
            CacheEntry {
                manifest,
                fetched_at: now,
            },
        );
        changed = true;
    }
    changed
}

/// Calls `fetch` for every key using a small pool of threads.
fn fetch_all(keys: &[String], fetch: &(impl Fn(&str) -> Fetched + Sync)) -> Vec<(String, Fetched)> {
    let next = AtomicUsize::new(0);
    let results = Mutex::new(Vec::with_capacity(keys.len()));
    thread::scope(|s| {
        for _ in 0..keys.len().min(MAX_PARALLEL_REQUESTS) {
            s.spawn(|| {
                while let Some(key) = keys.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let fetched = fetch(key);
                    results.lock().unwrap().push((key.clone(), fetched));
                }
            });
        }
    });
    results.into_inner().unwrap()
}

/// Writes the cache atomically; failures are ignored (the cache is optional).
fn save_cache(path: &Path, cache: &Cache) {
    let Some(dir) = path.parent() else { return };
    let Ok(json) = serde_json::to_string(cache) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    let _ = fs::create_dir_all(dir)
        .and_then(|_| fs::write(&tmp, json))
        .and_then(|_| fs::rename(&tmp, path));
}

/// `~/Library/Caches/nvmtree/registry.json` (macOS),
/// `%LOCALAPPDATA%\nvmtree\cache\registry.json` (Windows),
/// `$XDG_CACHE_HOME/nvmtree/registry.json` or `~/.cache/nvmtree/registry.json` (Linux).
pub fn default_cache_path() -> Option<PathBuf> {
    let env = |key| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let dir = if cfg!(windows) {
        env("LOCALAPPDATA")?.join("nvmtree").join("cache")
    } else if cfg!(target_os = "macos") {
        env("HOME")?.join("Library/Caches/nvmtree")
    } else {
        env("XDG_CACHE_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".cache")))?
            .join("nvmtree")
    };
    Some(dir.join("registry.json"))
}

/// Fetches manifests (`name/version` keys) and abbreviated version lists
/// (bare `name` keys) from `$npm_config_registry`, defaulting to the public
/// npm registry.
pub fn http_fetcher() -> impl Fn(&str) -> Fetched + Sync {
    let base = ["npm_config_registry", "NPM_CONFIG_REGISTRY"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| "https://registry.npmjs.org".to_string());
    let base = base.trim_end_matches('/').to_string();
    let agent = |timeout| -> ureq::Agent {
        ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .user_agent(concat!("nvmtree/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    };
    let (manifests, lists) = (agent(REQUEST_TIMEOUT), agent(VERSIONS_TIMEOUT));

    move |key| {
        let is_list = !key.contains('/');
        let request = if is_list {
            // The abbreviated format still has `engines` and `deprecated`.
            lists
                .get(&format!("{base}/{key}"))
                .header("Accept", "application/vnd.npm.install-v1+json")
        } else {
            manifests
                .get(&format!("{base}/{key}"))
                .header("Accept", "application/json")
        };
        let Ok(mut response) = request.call() else {
            return Fetched::Failed;
        };
        match response.status().as_u16() {
            200 => response
                .body_mut()
                .with_config()
                .limit(VERSIONS_BODY_LIMIT)
                .read_to_string()
                .ok()
                .and_then(|body| {
                    if is_list {
                        parse_versions(&body)
                    } else {
                        parse_manifest(&body)
                    }
                })
                .map_or(Fetched::Failed, Fetched::Found),
            404 => Fetched::Missing,
            _ => Fetched::Failed,
        }
    }
}

fn text(json: &serde_json::Value, key: &str) -> Option<String> {
    json.get(key)
        .and_then(|v| v.as_str())
        .map(|s| crate::sanitize::printable(s).into_owned())
        .filter(|s| !s.is_empty())
}

/// `engines.node`; old packages sometimes have `engines` as an array.
fn engines_node(json: &serde_json::Value) -> Option<String> {
    json.get("engines").and_then(|e| text(e, "node"))
}

fn parse_manifest(body: &str) -> Option<Manifest> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    Some(Manifest {
        version: text(&json, "version"),
        deprecated: text(&json, "deprecated"),
        engines_node: engines_node(&json),
        versions: Vec::new(),
    })
}

/// A version list: stable, non-deprecated versions with their `engines.node`.
fn parse_versions(body: &str) -> Option<Manifest> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let versions = json
        .get("versions")?
        .as_object()?
        .iter()
        .filter(|(v, _)| crate::nvm::parse_version(v).is_some() && is_safe_version(v))
        .filter(|(_, m)| text(m, "deprecated").is_none())
        .map(|(v, m)| Published {
            version: v.clone(),
            engines_node: engines_node(m),
        })
        .collect();
    Some(Manifest {
        versions,
        ..Manifest::default()
    })
}

/// Whether `candidate` is a newer release than `installed` (semver core
/// compared numerically; a release beats a prerelease of the same version).
pub fn is_newer(candidate: &str, installed: &str) -> bool {
    fn split(v: &str) -> Option<((u64, u64, u64), bool)> {
        let v = v.split('+').next()?;
        let (core, pre) = match v.split_once('-') {
            Some((core, _)) => (core, true),
            None => (v, false),
        };
        Some((crate::nvm::parse_version(core)?, pre))
    }
    match (split(candidate), split(installed)) {
        (Some((c, c_pre)), Some((i, i_pre))) => c > i || (c == i && i_pre && !c_pre),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use tempfile::TempDir;

    fn manifest(version: &str, deprecated: Option<&str>) -> Fetched {
        Fetched::Found(Manifest {
            version: Some(version.to_string()),
            deprecated: deprecated.map(str::to_string),
            ..Manifest::default()
        })
    }

    fn fake_registry(key: &str) -> Fetched {
        match key {
            "@shopify%2Fcli/latest" => manifest("4.9.3", None),
            "@shopify%2Fcli/4.8.4" => manifest("4.8.4", None),
            "request/latest" | "request/2.88.2" => {
                manifest("2.88.2", Some("request has been deprecated"))
            }
            "yoinks/latest" | "yoinks/0.3.1" => manifest("0.3.1", None),
            "private-thing/latest" | "private-thing/1.0.0" => Fetched::Missing,
            _ => Fetched::Failed,
        }
    }

    fn pkgs(list: &[(&str, &str)]) -> Vec<Query> {
        list.iter()
            .map(|(n, v)| (n.to_string(), v.to_string(), None))
            .collect()
    }

    fn info<'a>(lookup: &'a Lookup, name: &str, version: &str) -> &'a Info {
        &lookup.info[&(name.to_string(), version.to_string(), None)]
    }

    #[test]
    fn reports_updates_and_deprecations() {
        let packages = pkgs(&[
            ("@shopify/cli", "4.8.4"),
            ("request", "2.88.2"),
            ("yoinks", "0.3.1"),
            ("private-thing", "1.0.0"),
        ]);
        let result = lookup(&packages, None, CACHE_TTL, false, fake_registry);

        assert_eq!(
            info(&result, "@shopify/cli", "4.8.4").update.as_deref(),
            Some("4.9.3")
        );
        assert_eq!(info(&result, "@shopify/cli", "4.8.4").deprecated, None);
        assert_eq!(
            info(&result, "request", "2.88.2").deprecated.as_deref(),
            Some("request has been deprecated")
        );
        assert_eq!(info(&result, "request", "2.88.2").update, None);
        assert_eq!(info(&result, "yoinks", "0.3.1"), &Info::default());
        assert_eq!(info(&result, "private-thing", "1.0.0"), &Info::default());
        assert!(!result.incomplete);
    }

    #[test]
    fn uses_cache_and_survives_going_offline() {
        let tmp = TempDir::new().unwrap();
        let cache = tmp.path().join("sub/registry.json");
        let packages = pkgs(&[("@shopify/cli", "4.8.4")]);
        let calls = AtomicUsize::new(0);
        let counting = |key: &str| {
            calls.fetch_add(1, Ordering::Relaxed);
            fake_registry(key)
        };

        lookup(&packages, Some(&cache), CACHE_TTL, false, counting);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert!(cache.exists());

        // Fresh cache: no requests.
        let result = lookup(&packages, Some(&cache), CACHE_TTL, false, counting);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            info(&result, "@shopify/cli", "4.8.4").update.as_deref(),
            Some("4.9.3")
        );

        // --refresh refetches.
        lookup(&packages, Some(&cache), CACHE_TTL, true, counting);
        assert_eq!(calls.load(Ordering::Relaxed), 4);

        // Expired cache + registry unreachable: stale answers are still used.
        let result = lookup(&packages, Some(&cache), Duration::ZERO, false, |_: &str| {
            Fetched::Failed
        });
        assert_eq!(
            info(&result, "@shopify/cli", "4.8.4").update.as_deref(),
            Some("4.9.3")
        );
        assert!(!result.incomplete);
    }

    #[test]
    fn unreachable_registry_without_cache_is_incomplete() {
        let packages = pkgs(&[("yoinks", "0.3.1")]);
        let result = lookup(&packages, None, CACHE_TTL, false, |_: &str| Fetched::Failed);
        assert!(result.incomplete);
        assert_eq!(info(&result, "yoinks", "0.3.1"), &Info::default());
    }

    #[test]
    fn never_requests_unsafe_names_or_versions() {
        let packages = pkgs(&[
            ("yoinks", "0.3.1"),
            ("../../-/whoami", "1.0.0"),
            ("evil?x=1", "1.0.0"),
            ("yoinks2", "1.0.0#frag"),
        ]);
        let requested = Mutex::new(Vec::new());
        let result = lookup(&packages, None, CACHE_TTL, false, |key: &str| {
            requested.lock().unwrap().push(key.to_string());
            fake_registry(key)
        });
        let mut requested = requested.into_inner().unwrap();
        requested.sort();
        assert_eq!(requested, ["yoinks/0.3.1", "yoinks/latest"]);
        assert_eq!(result.info.len(), 1);
        assert!(!result.incomplete);
    }

    #[test]
    fn compares_versions() {
        assert!(is_newer("4.9.3", "4.8.4"));
        assert!(is_newer("10.0.0", "9.9.9"));
        assert!(is_newer("2.0.0", "2.0.0-beta.1"));
        assert!(!is_newer("4.8.4", "4.8.4"));
        assert!(!is_newer("4.8.4", "5.0.0-rc.1"));
        assert!(!is_newer("1.0.0", "1.0.1"));
        assert!(!is_newer("not-a-version", "1.0.0"));
    }

    #[test]
    fn parses_manifests() {
        assert_eq!(
            parse_manifest(r#"{"version":"2.88.2","deprecated":"use fetch"}"#),
            Some(Manifest {
                version: Some("2.88.2".into()),
                deprecated: Some("use fetch".into()),
                ..Manifest::default()
            })
        );
        // Empty deprecation messages mean "un-deprecated".
        assert_eq!(
            parse_manifest(r#"{"version":"1.0.0","deprecated":""}"#)
                .unwrap()
                .deprecated,
            None
        );
        assert_eq!(parse_manifest("not json"), None);
    }

    fn angular_registry(key: &str) -> Fetched {
        let with_engines = |version: &str, node: &str| {
            Fetched::Found(Manifest {
                version: Some(version.into()),
                engines_node: Some(node.into()),
                ..Manifest::default()
            })
        };
        let published = |version: &str, node: &str| Published {
            version: version.into(),
            engines_node: Some(node.into()),
        };
        match key {
            "@angular%2Fcli/latest" => with_engines("22.2.6", "^22.22.3 || ^24.15.0 || >=26.0.0"),
            "@angular%2Fcli/12.2.10" => with_engines("12.2.10", "^12.14.1 || >=14.0.0"),
            "@angular%2Fcli/22.0.0" => with_engines("22.0.0", "^22.22.3 || ^24.15.0"),
            "@angular%2Fcli" => Fetched::Found(Manifest {
                versions: vec![
                    published("12.2.18", "^12.14.1 || >=14.0.0"),
                    published("13.3.11", "^12.20.0 || ^14.15.0 || >=16.10.0"),
                    published("14.2.13", "^14.15.0 || >=16.10.0"),
                    published("22.2.6", "^22.22.3 || ^24.15.0 || >=26.0.0"),
                ],
                ..Manifest::default()
            }),
            _ => Fetched::Failed,
        }
    }

    #[test]
    fn picks_the_newest_version_for_the_node_version() {
        let old = (
            "@angular/cli".to_string(),
            "12.2.10".to_string(),
            Some((12, 22, 12)),
        );
        let new = (
            "@angular/cli".to_string(),
            "22.0.0".to_string(),
            Some((24, 15, 0)),
        );
        let requested = Mutex::new(Vec::new());
        let result = lookup(
            &[old.clone(), new.clone()],
            None,
            CACHE_TTL,
            false,
            |key: &str| {
                requested.lock().unwrap().push(key.to_string());
                angular_registry(key)
            },
        );

        let info = &result.info[&old];
        assert_eq!(info.update.as_deref(), Some("13.3.11"));
        let needs = info.latest_needs_node.as_ref().unwrap();
        assert_eq!(needs.latest, "22.2.6");
        assert_eq!(needs.min_node, Some((22, 22, 3)));
        assert!(!info.update_is_latest());

        let info = &result.info[&new];
        assert_eq!(info.update.as_deref(), Some("22.2.6"));
        assert!(info.update_is_latest());

        // The version list is fetched once, only because of the old one.
        let lists = requested.lock().unwrap();
        assert_eq!(lists.iter().filter(|k| *k == "@angular%2Fcli").count(), 1);
        assert!(!result.incomplete);
    }

    #[test]
    fn no_compatible_version_or_no_list_means_no_update() {
        let query = (
            "@angular/cli".to_string(),
            "12.2.10".to_string(),
            Some((10, 0, 0)),
        );
        let result = lookup(
            std::slice::from_ref(&query),
            None,
            CACHE_TTL,
            false,
            angular_registry,
        );
        let info = &result.info[&query];
        assert_eq!(info.update, None);
        assert_eq!(
            info.latest_needs_node.as_ref().unwrap().min_node,
            Some((22, 22, 3))
        );

        // Version list unreachable: still no update, and reported.
        let query = (
            "@angular/cli".to_string(),
            "12.2.10".to_string(),
            Some((12, 22, 12)),
        );
        let result = lookup(
            std::slice::from_ref(&query),
            None,
            CACHE_TTL,
            false,
            |key: &str| match key {
                "@angular%2Fcli" => Fetched::Failed,
                _ => angular_registry(key),
            },
        );
        assert_eq!(result.info[&query].update, None);
        assert!(result.incomplete);
    }

    #[test]
    fn parses_version_lists() {
        let body = r#"{"versions":{
            "1.0.0":{"engines":{"node":">=12"}},
            "1.1.0":{"engines":["node >= 0.8"]},
            "2.0.0-rc.1":{},
            "1.2.0":{"deprecated":"broken"}
        }}"#;
        let mut versions = parse_versions(body).unwrap().versions;
        versions.sort_by(|a, b| a.version.cmp(&b.version));
        assert_eq!(
            versions,
            [
                Published {
                    version: "1.0.0".into(),
                    engines_node: Some(">=12".into())
                },
                Published {
                    version: "1.1.0".into(),
                    engines_node: None
                },
            ]
        );
    }
}
