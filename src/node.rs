//! Node.js releases, from the `index.json` that nvm itself reads
//! (`nvm ls-remote`). Only fetched with `-u`; the request sends nothing about
//! what is installed. Cached like the registry answers.
//!
//! Updates are planned per release line (a major version, or `0.x` minor):
//! only the newest installed version of a line gets an update, so two
//! installed patches of 24.x never both report `↑ v24.22.2`. Older installed
//! versions of a line are "superseded" by the newest one.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::nvm::{Entry, Install, parse_version};
use crate::sanitize::printable;

pub const DEFAULT_MIRROR: &str = "https://nodejs.org/dist";
/// The index is ~350 KB (much less gzipped): allow more than registry calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One stable release (prereleases and nightlies are left out).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// `v24.22.2`, rebuilt from its numeric parts (safe to put in commands).
    pub version: String,
    /// LTS codename (`Krypton`) when the release is part of an LTS line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lts: Option<String>,
    /// Contains security fixes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub security: bool,
}

impl Release {
    pub fn parts(&self) -> (u64, u64, u64) {
        parse_version(&self.version).unwrap_or_default()
    }
}

/// What `-u` says about one installed Node version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The newest installed version of its line, and the line has a newer
    /// release. `security` when a release in between has security fixes.
    Update { to: String, security: bool },
    /// A newer version of the same line is installed (`by`). `missing`
    /// counts the global packages this one has that `by` doesn't.
    Superseded { by: String, missing: usize },
}

/// The result of comparing installed versions with the release list.
#[derive(Debug, Default)]
pub struct Plan {
    /// Keyed by installed version name (`v24.22.0`). Versions that are up to
    /// date (or unknown to the index) have no entry.
    pub status: HashMap<String, Status>,
    /// Newest LTS release.
    pub latest_lts: Option<Release>,
    /// Newest release overall ("Current").
    pub latest: Option<Release>,
    /// `latest_lts` / `latest` when their line is newer than every installed
    /// one (deduplicated, newest first).
    pub new_lines: Vec<Release>,
}

/// Release line of a version: the major, or `0.minor` for pre-1.0 Node.
fn line((major, minor, _): (u64, u64, u64)) -> (u64, u64) {
    if major == 0 { (0, minor) } else { (major, 0) }
}

/// Plans updates for `entries` (every installed version, not just the shown
/// ones, so a filter can't hide a newer installed patch). `releases` is
/// `None` when the index couldn't be loaded: then only superseded versions
/// are reported.
pub fn plan(entries: &[Entry], releases: Option<&[Release]>) -> Plan {
    let mut plan = Plan::default();

    // Newest installed version of each line.
    let mut newest: HashMap<(u64, u64), &Entry> = HashMap::new();
    for entry in entries {
        let current = newest.entry(line(entry.version.parts)).or_insert(entry);
        if entry.version.parts > current.version.parts {
            *current = entry;
        }
    }

    for entry in entries {
        let top = newest[&line(entry.version.parts)];
        if top.version.name != entry.version.name {
            let d = crate::compare::diff(&entry.packages, &top.packages);
            plan.status.insert(
                entry.version.name.clone(),
                Status::Superseded {
                    by: top.version.name.clone(),
                    missing: d.only_from.len(),
                },
            );
        }
    }

    let Some(releases) = releases else {
        return plan;
    };
    for (key, top) in &newest {
        let installed = top.version.parts;
        let newer: Vec<&Release> = releases
            .iter()
            .filter(|r| line(r.parts()) == *key && r.parts() > installed)
            .collect();
        if let Some(target) = newer.iter().max_by_key(|r| r.parts()) {
            plan.status.insert(
                top.version.name.clone(),
                Status::Update {
                    to: target.version.clone(),
                    security: newer.iter().any(|r| r.security),
                },
            );
        }
    }

    plan.latest = releases.iter().max_by_key(|r| r.parts()).cloned();
    plan.latest_lts = releases
        .iter()
        .filter(|r| r.lts.is_some())
        .max_by_key(|r| r.parts())
        .cloned();
    let newest_installed = newest.keys().max().copied();
    for release in [&plan.latest, &plan.latest_lts].into_iter().flatten() {
        let is_new = newest_installed.is_none_or(|n| line(release.parts()) > n);
        if is_new && !plan.new_lines.contains(release) {
            plan.new_lines.push(release.clone());
        }
    }
    plan
}

/// URL of the release index: `<mirror>/index.json`.
pub fn index_url(install: &Install) -> String {
    let mirror = install.node_mirror.as_deref().unwrap_or(DEFAULT_MIRROR);
    format!("{}/index.json", mirror.trim().trim_end_matches('/'))
}

/// Parses nodejs.org's `index.json`, keeping stable releases, newest first.
pub fn parse_index(body: &str) -> Option<Vec<Release>> {
    let json: Vec<serde_json::Value> = serde_json::from_str(body).ok()?;
    let mut releases: Vec<Release> = json
        .iter()
        .filter_map(|r| {
            let (a, b, c) = parse_version(r.get("version")?.as_str()?)?;
            let lts = r
                .get("lts")
                .and_then(|v| v.as_str())
                .map(|s| printable(s).into_owned())
                .filter(|s| {
                    !s.is_empty() && s.len() <= 32 && s.chars().all(|c| c.is_ascii_alphanumeric())
                });
            Some(Release {
                version: format!("v{a}.{b}.{c}"),
                lts,
                security: r.get("security").and_then(|v| v.as_bool()) == Some(true),
            })
        })
        .collect();
    releases.sort_by_key(|r| std::cmp::Reverse(r.parts()));
    releases.dedup_by_key(|r| r.parts());
    Some(releases)
}

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    url: String,
    fetched_at: u64,
    releases: Vec<Release>,
}

/// The release list from `url`, reusing the cache at `cache_path` when it's
/// younger than `ttl` (unless `refresh`). Falls back to a stale cache when
/// `fetch` fails; `None` when there's nothing at all.
pub fn releases(
    url: &str,
    cache_path: Option<&Path>,
    ttl: Duration,
    refresh: bool,
    fetch: impl FnOnce(&str) -> Option<String>,
) -> Option<Vec<Release>> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let cached: Option<Cache> = cache_path
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .filter(|c: &Cache| c.url == url && !c.releases.is_empty());
    if let Some(cache) = &cached
        && !refresh
        && now.saturating_sub(cache.fetched_at) < ttl.as_secs()
    {
        return cached.map(|c| c.releases);
    }

    match fetch(url).as_deref().and_then(parse_index) {
        Some(releases) if !releases.is_empty() => {
            if let Some(path) = cache_path {
                save_cache(
                    path,
                    &Cache {
                        url: url.to_string(),
                        fetched_at: now,
                        releases: releases.clone(),
                    },
                );
            }
            Some(releases)
        }
        // Better slightly stale than nothing.
        _ => cached.map(|c| c.releases),
    }
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

/// `node-releases.json` next to the registry cache.
pub fn default_cache_path() -> Option<std::path::PathBuf> {
    Some(
        crate::registry::default_cache_path()?
            .parent()?
            .join("node-releases.json"),
    )
}

/// Downloads `url`; `None` on any error or non-200 answer.
pub fn http_fetch(url: &str) -> Option<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        .http_status_as_error(false)
        .user_agent(concat!("nvmtree/", env!("CARGO_PKG_VERSION")))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .header("Accept", "application/json")
        .call()
        .ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    response
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_string()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvm::{GlobalPackage, NodeVersion};
    use std::cell::Cell;
    use std::path::PathBuf;
    use tempfile::TempDir;

    const INDEX: &str = r#"[
        {"version":"v27.0.0-alpha.0.0.0","lts":false,"security":false},
        {"version":"v26.13.0","lts":"Lithium","security":false},
        {"version":"v26.12.1","lts":false,"security":true},
        {"version":"v24.22.2","lts":"Krypton","security":false},
        {"version":"v24.22.1","lts":"Krypton","security":true},
        {"version":"v24.22.0","lts":"Krypton","security":false},
        {"version":"v22.21.1","lts":"Jod\u001b[2J","security":false},
        {"version":"v22.21.0","lts":"Jod","security":false},
        {"version":"v0.12.18","lts":false,"security":false},
        {"version":"$(rm -rf ~)","lts":false},
        {"lts":false}
    ]"#;

    fn pkg(name: &str) -> GlobalPackage {
        GlobalPackage {
            name: name.to_string(),
            version: Some("1.0.0".to_string()),
            ..GlobalPackage::default()
        }
    }

    fn entry(name: &str, packages: &[&str]) -> Entry {
        Entry {
            version: NodeVersion {
                name: name.to_string(),
                parts: parse_version(name).unwrap(),
                path: PathBuf::from(name),
            },
            label: None,
            packages: packages.iter().map(|p| pkg(p)).collect(),
            node: None,
        }
    }

    fn release(version: &str, lts: Option<&str>) -> Release {
        Release {
            version: version.to_string(),
            lts: lts.map(str::to_string),
            security: false,
        }
    }

    #[test]
    fn parses_stable_releases_only() {
        let releases = parse_index(INDEX).unwrap();
        let versions: Vec<&str> = releases.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(
            versions,
            [
                "v26.13.0", "v26.12.1", "v24.22.2", "v24.22.1", "v24.22.0", "v22.21.1", "v22.21.0",
                "v0.12.18"
            ]
        );
        assert_eq!(releases[0].lts.as_deref(), Some("Lithium"));
        assert!(releases[1].security);
        // A codename with control characters is dropped, not printed.
        assert_eq!(releases[5].lts, None);
        assert_eq!(parse_index("{}"), None);
    }

    #[test]
    fn one_update_per_line_and_older_patches_are_superseded() {
        let releases = parse_index(INDEX).unwrap();
        let entries = [
            entry("v24.22.1", &["typescript"]),
            entry("v24.22.0", &["typescript", "pnpm"]),
            entry("v24.21.0", &["typescript"]),
            entry("v22.21.1", &[]),
        ];
        let plan = plan(&entries, Some(&releases));

        assert_eq!(
            plan.status["v24.22.1"],
            Status::Update {
                to: "v24.22.2".into(),
                security: false
            }
        );
        // Not a second `↑ v24.22.2`: they're older copies of the same line.
        assert_eq!(
            plan.status["v24.22.0"],
            Status::Superseded {
                by: "v24.22.1".into(),
                missing: 1
            }
        );
        assert_eq!(
            plan.status["v24.21.0"],
            Status::Superseded {
                by: "v24.22.1".into(),
                missing: 0
            }
        );
        // Up to date.
        assert!(!plan.status.contains_key("v22.21.1"));

        // 26 is newer than every installed line: both LTS and Current.
        assert_eq!(plan.new_lines, [release("v26.13.0", Some("Lithium"))]);
        assert_eq!(plan.latest, plan.latest_lts);
    }

    #[test]
    fn flags_security_releases_in_between() {
        let releases = parse_index(INDEX).unwrap();
        let plan = plan(&[entry("v24.22.0", &[])], Some(&releases));
        assert_eq!(
            plan.status["v24.22.0"],
            Status::Update {
                to: "v24.22.2".into(),
                security: true
            }
        );
    }

    #[test]
    fn new_lines_dedupe_lts_and_current() {
        let releases = vec![
            release("v27.1.0", None),
            release("v26.13.0", Some("Lithium")),
            release("v24.22.2", Some("Krypton")),
        ];
        let p = plan(&[entry("v24.22.2", &[])], Some(&releases));
        assert!(p.status.is_empty());
        assert_eq!(
            p.new_lines,
            [
                release("v27.1.0", None),
                release("v26.13.0", Some("Lithium"))
            ]
        );
        // Having 26 installed: only the newer Current line is new.
        let p = plan(&[entry("v26.1.0", &[])], Some(&releases));
        assert_eq!(p.new_lines, [release("v27.1.0", None)]);
        // Nothing to suggest when the newest line is installed.
        let p = plan(&[entry("v27.0.0", &[])], Some(&releases));
        assert!(p.new_lines.is_empty());
    }

    #[test]
    fn without_releases_only_reports_superseded() {
        let entries = [entry("v24.22.1", &[]), entry("v24.22.0", &[])];
        let p = plan(&entries, None);
        assert_eq!(p.status.len(), 1);
        assert!(matches!(p.status["v24.22.0"], Status::Superseded { .. }));
        assert!(p.new_lines.is_empty());
    }

    #[test]
    fn pre_1_0_minors_are_separate_lines() {
        let releases = vec![release("v0.12.18", None), release("v0.10.48", None)];
        let p = plan(
            &[entry("v0.12.18", &[]), entry("v0.10.40", &[])],
            Some(&releases),
        );
        assert_eq!(
            p.status["v0.10.40"],
            Status::Update {
                to: "v0.10.48".into(),
                security: false
            }
        );
        assert!(!p.status.contains_key("v0.12.18"));
    }

    #[test]
    fn caches_and_falls_back_to_stale_cache() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("cache").join("node-releases.json");
        let url = "https://nodejs.org/dist/index.json";
        let calls = Cell::new(0);
        let online = |_: &str| {
            calls.set(calls.get() + 1);
            Some(INDEX.to_string())
        };
        let offline = |_: &str| {
            calls.set(calls.get() + 1);
            None
        };
        let ttl = Duration::from_secs(3600);

        let first = releases(url, Some(&path), ttl, false, online).unwrap();
        assert_eq!(calls.get(), 1);
        // Fresh cache: no request.
        assert_eq!(
            releases(url, Some(&path), ttl, false, online),
            Some(first.clone())
        );
        assert_eq!(calls.get(), 1);
        // --refresh asks again; when that fails the cache is still used.
        assert_eq!(
            releases(url, Some(&path), ttl, true, offline),
            Some(first.clone())
        );
        assert_eq!(calls.get(), 2);
        // Expired cache + offline: stale answer.
        assert_eq!(
            releases(url, Some(&path), Duration::ZERO, false, offline),
            Some(first)
        );
        // Another mirror doesn't reuse this cache.
        assert_eq!(
            releases(
                "https://mirror/index.json",
                Some(&path),
                ttl,
                false,
                offline
            ),
            None
        );
    }

    #[test]
    fn index_url_uses_the_mirror() {
        let mut install = Install::nvm_sh(PathBuf::from("/nvm"));
        assert_eq!(index_url(&install), "https://nodejs.org/dist/index.json");
        install.node_mirror = Some("https://npmmirror.com/mirrors/node/".into());
        assert_eq!(
            index_url(&install),
            "https://npmmirror.com/mirrors/node/index.json"
        );
    }
}
