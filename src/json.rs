//! `--json` output: one JSON document per command, for scripts and other
//! tools. These structs are the public format, kept separate from the
//! internal types so refactoring can't silently change it. Bump
//! [`SCHEMA_VERSION`] on any breaking change (renamed/removed fields, changed
//! meaning); adding fields is fine.

use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::compare::{Diff, Found};
use crate::node::{Plan, Status};
use crate::npx::NpxPackage;
use crate::nvm::{Entry, Flavor, GlobalPackage, Install};

pub const SCHEMA_VERSION: u32 = 1;

/// A global package. `update` / `deprecated` / `latest_needs_node` are only
/// filled in when the registry was checked (`-u`); otherwise they're `null`.
/// `update` is the newest version that runs on this Node version.
#[derive(Serialize)]
pub struct Package<'a> {
    name: &'a str,
    version: Option<&'a str>,
    linked: bool,
    bins: &'a [String],
    update: Option<&'a str>,
    deprecated: Option<&'a str>,
    latest_needs_node: Option<NeedsNode<'a>>,
}

/// The registry's `latest` doesn't run on this Node version.
#[derive(Serialize)]
pub struct NeedsNode<'a> {
    /// The `latest` version.
    version: &'a str,
    /// Its `engines.node` range.
    engines_node: &'a str,
    /// The oldest newer Node version that satisfies it, if any.
    min_node: Option<String>,
}

impl<'a> From<&'a GlobalPackage> for Package<'a> {
    fn from(p: &'a GlobalPackage) -> Self {
        Package {
            name: &p.name,
            version: p.version.as_deref(),
            linked: p.linked,
            bins: &p.bins,
            update: p.registry.update.as_deref(),
            deprecated: p.registry.deprecated.as_deref(),
            latest_needs_node: p.registry.latest_needs_node.as_ref().map(|n| NeedsNode {
                version: &n.latest,
                engines_node: &n.engines_node,
                min_node: n.min_node.map(|(a, b, c)| format!("{a}.{b}.{c}")),
            }),
        }
    }
}

/// A Node version without its packages: `{"version": "24.21.0", "label": "default"}`.
#[derive(Serialize)]
pub struct NodeRef<'a> {
    version: &'a str,
    /// `"default"` (nvm) or `"current"` (nvm-windows) for the version nvm uses.
    label: Option<&'static str>,
}

impl<'a> From<&'a Entry> for NodeRef<'a> {
    fn from(e: &'a Entry) -> Self {
        NodeRef {
            version: node_version(e),
            label: e.label,
        }
    }
}

fn node_version(e: &Entry) -> &str {
    e.version.name.trim_start_matches('v')
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[derive(Serialize)]
struct Nvm {
    /// `"nvm"` (nvm-sh, macOS/Linux) or `"nvm-windows"`.
    kind: &'static str,
    root: String,
}

impl From<&Install> for Nvm {
    fn from(install: &Install) -> Self {
        Nvm {
            kind: match install.flavor {
                Flavor::NvmSh => "nvm",
                Flavor::NvmWindows { .. } => "nvm-windows",
            },
            root: path_string(&install.root),
        }
    }
}

/// A newer release of a version's line (only on the newest installed
/// version of each line).
#[derive(Serialize)]
struct NodeUpdate<'a> {
    version: &'a str,
    /// A release between the installed one and `version` has security fixes.
    security: bool,
}

#[derive(Serialize)]
struct Version<'a> {
    version: &'a str,
    label: Option<&'static str>,
    path: String,
    /// With `-u`: newer release of this line, else `null`.
    node_update: Option<NodeUpdate<'a>>,
    /// With `-u`: newest installed version of the same line when this one
    /// is older, else `null`.
    superseded_by: Option<&'a str>,
    packages: Vec<Package<'a>>,
}

/// Newest Node releases, from the release index (`-u`).
#[derive(Serialize)]
struct NodeReleases<'a> {
    latest: Option<&'a str>,
    latest_lts: Option<&'a str>,
    lts_codename: Option<&'a str>,
}

/// `nvmtree --json`: every (selected) Node version and its global packages.
#[derive(Serialize)]
pub struct List<'a> {
    schema_version: u32,
    nvm: Nvm,
    /// Whether the npm registry was asked (`-u`); when false, every
    /// package's `update` and `deprecated` are `null`.
    updates_checked: bool,
    /// The registry couldn't be reached for some packages.
    registry_incomplete: bool,
    /// With `-u`: newest Node releases; `null` when not checked or the
    /// release list couldn't be loaded.
    node_releases: Option<NodeReleases<'a>>,
    /// The Node release list couldn't be loaded (`node_update` is `null`).
    node_releases_unreachable: bool,
    versions: Vec<Version<'a>>,
}

/// `node`: the update plan and whether the release list was loaded, when
/// checked (`-u`).
pub fn list<'a>(
    install: &Install,
    entries: &'a [Entry],
    updates_checked: bool,
    registry_incomplete: bool,
    node: Option<(&'a Plan, bool)>,
) -> List<'a> {
    let bare = |v: &'a str| v.trim_start_matches('v');
    List {
        schema_version: SCHEMA_VERSION,
        nvm: install.into(),
        updates_checked,
        registry_incomplete,
        node_releases: node
            .filter(|(_, reached)| *reached)
            .map(|(plan, _)| NodeReleases {
                latest: plan.latest.as_ref().map(|r| bare(&r.version)),
                latest_lts: plan.latest_lts.as_ref().map(|r| bare(&r.version)),
                lts_codename: plan.latest_lts.as_ref().and_then(|r| r.lts.as_deref()),
            }),
        node_releases_unreachable: node.is_some_and(|(_, reached)| !reached),
        versions: entries
            .iter()
            .map(|e| Version {
                version: node_version(e),
                label: e.label,
                path: path_string(&e.version.path),
                node_update: match &e.node {
                    Some(Status::Update { to, security }) => Some(NodeUpdate {
                        version: bare(to),
                        security: *security,
                    }),
                    _ => None,
                },
                superseded_by: match &e.node {
                    Some(Status::Superseded { by, .. }) => Some(bare(by)),
                    _ => None,
                },
                packages: e.packages.iter().map(Package::from).collect(),
            })
            .collect(),
    }
}

#[derive(Serialize)]
struct Changed<'a> {
    name: &'a str,
    from: Option<&'a str>,
    to: Option<&'a str>,
}

#[derive(Serialize)]
struct NotIncluded<'a> {
    /// Installed with `npm link`: must be re-linked by hand.
    linked: Vec<&'a str>,
    /// Name or version failed validation, so it was left out of the command.
    invalid: Vec<&'a str>,
}

/// `nvmtree diff FROM TO --json`.
#[derive(Serialize)]
pub struct DiffDoc<'a> {
    schema_version: u32,
    from: NodeRef<'a>,
    to: NodeRef<'a>,
    only_in_from: Vec<Package<'a>>,
    only_in_to: Vec<Package<'a>>,
    changed: Vec<Changed<'a>>,
    /// Number of packages with the same version in both.
    identical: usize,
    /// Shell command installing `only_in_from` into `to`, or `null`.
    install_command: Option<&'a str>,
    not_included: NotIncluded<'a>,
}

pub fn diff<'a>(
    from: &'a Entry,
    to: &'a Entry,
    d: &Diff<'a>,
    install_command: Option<&'a str>,
    invalid: &[&'a GlobalPackage],
) -> DiffDoc<'a> {
    DiffDoc {
        schema_version: SCHEMA_VERSION,
        from: from.into(),
        to: to.into(),
        only_in_from: d.only_from.iter().map(|p| Package::from(*p)).collect(),
        only_in_to: d.only_to.iter().map(|p| Package::from(*p)).collect(),
        changed: d
            .changed
            .iter()
            .map(|(f, t)| Changed {
                name: &f.name,
                from: f.version.as_deref(),
                to: t.version.as_deref(),
            })
            .collect(),
        identical: d.same,
        install_command,
        not_included: NotIncluded {
            linked: d
                .only_from
                .iter()
                .filter(|p| p.linked)
                .map(|p| p.name.as_str())
                .collect(),
            invalid: invalid.iter().map(|p| p.name.as_str()).collect(),
        },
    }
}

#[derive(Serialize)]
struct Installed<'a> {
    node: NodeRef<'a>,
    version: Option<&'a str>,
    linked: bool,
}

#[derive(Serialize)]
struct FoundPackage<'a> {
    name: &'a str,
    bins: &'a [&'a str],
    installed: Vec<Installed<'a>>,
    /// Node versions that don't have it.
    missing: Vec<NodeRef<'a>>,
}

#[derive(Serialize)]
struct CachedPackage<'a> {
    name: &'a str,
    version: Option<&'a str>,
    bins: &'a [String],
    path: String,
    /// When npx last used it, as Unix seconds.
    last_used: Option<u64>,
}

/// `nvmtree which NAME --json`.
#[derive(Serialize)]
pub struct WhichDoc<'a> {
    schema_version: u32,
    query: &'a str,
    /// Global packages matching by name or command.
    packages: Vec<FoundPackage<'a>>,
    /// Matches in the npx cache (not installed globally).
    npx_cache: Vec<CachedPackage<'a>>,
    /// Similar names, only when nothing matched.
    suggestions: Vec<&'a str>,
}

impl WhichDoc<'_> {
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty() && self.npx_cache.is_empty()
    }
}

pub fn which<'a>(
    query: &'a str,
    found: &'a [Found<'a>],
    npx: &[&'a NpxPackage],
    suggestions: Vec<&'a str>,
) -> WhichDoc<'a> {
    WhichDoc {
        schema_version: SCHEMA_VERSION,
        query,
        packages: found
            .iter()
            .map(|f| FoundPackage {
                name: f.name,
                bins: &f.bins,
                installed: f
                    .installed
                    .iter()
                    .map(|(e, p)| Installed {
                        node: (*e).into(),
                        version: p.version.as_deref(),
                        linked: p.linked,
                    })
                    .collect(),
                missing: f.missing.iter().map(|e| (*e).into()).collect(),
            })
            .collect(),
        npx_cache: npx
            .iter()
            .map(|p| CachedPackage {
                name: &p.package.name,
                version: p.package.version.as_deref(),
                bins: &p.package.bins,
                path: path_string(&p.dir),
                last_used: p
                    .used
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs()),
            })
            .collect(),
        suggestions,
    }
}

/// Prints `doc` as pretty JSON on stdout (plain `println!`: never styled).
pub fn print(doc: &impl Serialize) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(doc)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvm::NodeVersion;
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime};

    fn pkg(name: &str, version: &str, bins: &[&str], linked: bool) -> GlobalPackage {
        GlobalPackage {
            name: name.to_string(),
            version: Some(version.to_string()),
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
                path: PathBuf::from(format!("/nvm/versions/node/{name}")),
            },
            label,
            packages,
            node: None,
        }
    }

    fn to_value(doc: &impl Serialize) -> Value {
        serde_json::to_value(doc).unwrap()
    }

    #[test]
    fn list_document() {
        let install = Install::nvm_sh(PathBuf::from("/nvm"));
        let mut shopify = pkg("@shopify/cli", "4.8.4", &["shopify"], false);
        shopify.registry.update = Some("4.9.3".into());
        let mut entries = vec![
            entry(
                "v26.11.1",
                None,
                vec![pkg("typescript", "7.0.2", &["tsc"], false)],
            ),
            entry("v24.21.0", Some("default"), vec![shopify]),
        ];
        entries[0].node = Some(Status::Update {
            to: "v26.13.0".into(),
            security: true,
        });
        entries[1].node = Some(Status::Superseded {
            by: "v24.22.0".into(),
            missing: 0,
        });
        let release = |version: &str, lts: Option<&str>| crate::node::Release {
            version: version.into(),
            lts: lts.map(str::to_string),
            security: false,
        };
        let plan = Plan {
            latest: Some(release("v27.1.0", None)),
            latest_lts: Some(release("v26.13.0", Some("Lithium"))),
            ..Plan::default()
        };
        assert_eq!(
            to_value(&list(&install, &entries, true, false, Some((&plan, true)))),
            json!({
                "schema_version": 1,
                "nvm": {"kind": "nvm", "root": "/nvm"},
                "updates_checked": true,
                "registry_incomplete": false,
                "node_releases": {
                    "latest": "27.1.0", "latest_lts": "26.13.0", "lts_codename": "Lithium"
                },
                "node_releases_unreachable": false,
                "versions": [
                    {
                        "version": "26.11.1",
                        "label": null,
                        "path": "/nvm/versions/node/v26.11.1",
                        "node_update": {"version": "26.13.0", "security": true},
                        "superseded_by": null,
                        "packages": [{
                            "name": "typescript", "version": "7.0.2", "linked": false,
                            "bins": ["tsc"], "update": null, "deprecated": null,
                            "latest_needs_node": null
                        }]
                    },
                    {
                        "version": "24.21.0",
                        "label": "default",
                        "path": "/nvm/versions/node/v24.21.0",
                        "node_update": null,
                        "superseded_by": "24.22.0",
                        "packages": [{
                            "name": "@shopify/cli", "version": "4.8.4", "linked": false,
                            "bins": ["shopify"], "update": "4.9.3", "deprecated": null,
                            "latest_needs_node": null
                        }]
                    }
                ]
            })
        );
        // Release list unreachable: no releases, flagged.
        let doc = to_value(&list(&install, &entries, true, false, Some((&plan, false))));
        assert_eq!(doc["node_releases"], Value::Null);
        assert_eq!(doc["node_releases_unreachable"], true);
        // Not checked: nothing.
        let doc = to_value(&list(&install, &[], false, false, None));
        assert_eq!(doc["node_releases"], Value::Null);
        assert_eq!(doc["node_releases_unreachable"], false);

        let windows = Install::nvm_windows(Path::new("/nope"), None);
        assert_eq!(
            to_value(&list(&windows, &[], false, false, None))["nvm"]["kind"],
            "nvm-windows"
        );
    }

    #[test]
    fn diff_document() {
        let from = entry(
            "v24.0.0",
            Some("default"),
            vec![
                pkg("@shopify/cli", "4.8.4", &[], false),
                pkg("evil;name", "1.0.0", &[], false),
                pkg("mytool", "1.0.0", &[], true),
                pkg("npm", "11.19.0", &[], false),
                pkg("pnpm", "10.0.0", &[], false),
            ],
        );
        let to = entry(
            "v26.0.0",
            None,
            vec![
                pkg("npm", "11.20.0", &[], false),
                pkg("pnpm", "10.0.0", &[], false),
                pkg("typescript", "7.0.2", &["tsc"], false),
            ],
        );
        let d = crate::compare::diff(&from.packages, &to.packages);
        let invalid = crate::compare::not_installable(&d.only_from, false);
        let command = "nvm exec 26.0.0 npm i -g @shopify/cli";
        let doc = to_value(&diff(&from, &to, &d, Some(command), &invalid));
        assert_eq!(
            doc["from"],
            json!({"version": "24.0.0", "label": "default"})
        );
        assert_eq!(doc["to"], json!({"version": "26.0.0", "label": null}));
        let names = |key: &str| -> Vec<String> {
            doc[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            names("only_in_from"),
            ["@shopify/cli", "evil;name", "mytool"]
        );
        assert_eq!(names("only_in_to"), ["typescript"]);
        assert_eq!(
            doc["changed"],
            json!([{"name": "npm", "from": "11.19.0", "to": "11.20.0"}])
        );
        assert_eq!(doc["identical"], 1);
        assert_eq!(doc["install_command"], command);
        assert_eq!(
            doc["not_included"],
            json!({"linked": ["mytool"], "invalid": ["evil;name"]})
        );
    }

    #[test]
    fn which_document() {
        let entries = vec![
            entry(
                "v26.0.0",
                None,
                vec![pkg("typescript", "7.0.2", &["tsc"], false)],
            ),
            entry("v24.0.0", Some("default"), vec![]),
        ];
        let found = crate::compare::find(&entries, "tsc");
        let cached = NpxPackage {
            package: pkg("typescript", "5.0.0", &["tsc"], false),
            dir: PathBuf::from("/npm/_npx/abc"),
            used: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        };
        let doc = to_value(&which("tsc", &found, &[&cached], vec![]));
        assert_eq!(
            doc,
            json!({
                "schema_version": 1,
                "query": "tsc",
                "packages": [{
                    "name": "typescript",
                    "bins": ["tsc"],
                    "installed": [{
                        "node": {"version": "26.0.0", "label": null},
                        "version": "7.0.2",
                        "linked": false
                    }],
                    "missing": [{"version": "24.0.0", "label": "default"}]
                }],
                "npx_cache": [{
                    "name": "typescript", "version": "5.0.0", "bins": ["tsc"],
                    "path": "/npm/_npx/abc", "last_used": 1_700_000_000
                }],
                "suggestions": []
            })
        );

        let none = which("zzz", &[], &[], vec!["zz-top"]);
        assert!(none.is_empty());
        assert_eq!(to_value(&none)["suggestions"], json!(["zz-top"]));
    }

    #[test]
    fn control_characters_are_escaped() {
        let install = Install::nvm_sh(PathBuf::from("/nvm"));
        let entries = vec![entry(
            "v22.0.0",
            None,
            vec![pkg("evil\u{1b}]0;x\u{7}", "1.0.0", &[], false)],
        )];
        let text = serde_json::to_string(&list(&install, &entries, false, false, None)).unwrap();
        assert!(
            !text.contains('\u{1b}') && !text.contains('\u{7}'),
            "{text}"
        );
        assert!(text.contains(r"evil\u001b]0;x\u0007"), "{text}");
    }
}
