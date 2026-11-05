//! Matches Node versions against npm `engines.node` ranges
//! (`^22.22.3 || ^24.15.0 || >=26.0.0`, `>=14`, `12.x`, `1.2 - 2.3`, ...).
//!
//! Each `||` alternative is turned into a half-open interval `[low, high)` of
//! release versions. Prerelease tags are ignored, which is precise enough for
//! released Node versions. Anything that doesn't parse gives `None`, so callers
//! can treat an unknown range as "compatible" instead of hiding updates.

type Version = (u64, u64, u64);

/// `[low, high)`; `high == None` means no upper bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Interval {
    low: Version,
    high: Option<Version>,
}

impl Interval {
    const ANY: Interval = Interval {
        low: (0, 0, 0),
        high: None,
    };

    fn contains(&self, v: Version) -> bool {
        v >= self.low && self.high.is_none_or(|h| v < h)
    }

    fn is_empty(&self) -> bool {
        self.high.is_some_and(|h| h <= self.low)
    }

    fn intersect(self, other: Interval) -> Interval {
        Interval {
            low: self.low.max(other.low),
            high: match (self.high, other.high) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
        }
    }
}

/// A version with possibly missing parts: `1`, `1.2`, `1.2.3`, `1.x`, `*`.
#[derive(Debug, Clone, Copy)]
struct Partial {
    major: Option<u64>,
    minor: Option<u64>,
    patch: Option<u64>,
}

impl Partial {
    fn parse(s: &str) -> Option<Partial> {
        let s = s.strip_prefix(['v', '=']).unwrap_or(s);
        // Prerelease / build metadata: compare on the core version only.
        let s = s.split(['-', '+']).next()?;
        let mut parts = s.split('.');
        let mut next = || -> Option<Option<u64>> {
            match parts.next() {
                None | Some("x" | "X" | "*") => Some(None),
                Some(p) => p.parse().ok().map(Some),
            }
        };
        let (major, minor, patch) = (next()?, next()?, next()?);
        if parts.next().is_some() {
            return None;
        }
        // `1.x.3` is not a thing: once a part is missing, the rest must be too.
        let partial = Partial {
            major,
            minor: major.and(minor),
            patch: major.and(minor).and(patch),
        };
        Some(partial)
    }

    /// Lowest version matching it: `1.2` → `1.2.0`.
    fn start(&self) -> Version {
        (
            self.major.unwrap_or(0),
            self.minor.unwrap_or(0),
            self.patch.unwrap_or(0),
        )
    }

    /// First version past it: `1.2` → `1.3.0`, `1.2.3` → `1.2.4`, `*` → none.
    fn end(&self) -> Option<Version> {
        match (self.major, self.minor, self.patch) {
            (None, _, _) => None,
            (Some(m), None, _) => Some((m + 1, 0, 0)),
            (Some(m), Some(n), None) => Some((m, n + 1, 0)),
            (Some(m), Some(n), Some(p)) => Some((m, n, p + 1)),
        }
    }

    fn exact(&self) -> Interval {
        Interval {
            low: self.start(),
            high: self.end(),
        }
    }
}

fn comparator(op: &str, v: Partial) -> Interval {
    let (major, minor) = (v.major.unwrap_or(0), v.minor.unwrap_or(0));
    match op {
        ">=" => Interval {
            low: v.start(),
            high: None,
        },
        ">" => match v.end() {
            Some(low) => Interval { low, high: None },
            // `>*` matches nothing.
            None => Interval {
                low: (0, 0, 0),
                high: Some((0, 0, 0)),
            },
        },
        "<" => Interval {
            low: (0, 0, 0),
            high: Some(v.start()),
        },
        "<=" => Interval {
            low: (0, 0, 0),
            high: v.end(),
        },
        "~" | "~>" => Interval {
            low: v.start(),
            high: match v.minor {
                Some(_) => v.major.map(|_| (major, minor + 1, 0)),
                None => v.end(),
            },
        },
        "^" => Interval {
            low: v.start(),
            high: match (v.major, v.minor, v.patch) {
                (None, _, _) => None,
                (Some(0), Some(0), Some(p)) => Some((0, 0, p + 1)),
                (Some(0), Some(n), _) => Some((0, n + 1, 0)),
                (Some(m), _, _) => Some((m + 1, 0, 0)),
            },
        },
        _ => v.exact(),
    }
}

/// The intervals of a range, one per `||` alternative (empty ones dropped).
fn parse(range: &str) -> Option<Vec<Interval>> {
    let mut sets = Vec::new();
    for alternative in range.split("||") {
        let tokens: Vec<&str> = alternative.split_whitespace().collect();
        let interval = match tokens[..] {
            [] => Interval::ANY,
            [from, "-", to] => Interval {
                low: Partial::parse(from)?.start(),
                high: Partial::parse(to)?.end(),
            },
            _ => {
                let mut interval = Interval::ANY;
                let mut tokens = tokens.into_iter();
                while let Some(token) = tokens.next() {
                    let split = token
                        .find(|c: char| !matches!(c, '<' | '>' | '=' | '~' | '^'))
                        .unwrap_or(token.len());
                    let (op, mut version) = token.split_at(split);
                    // `>= 1.2.3`: the operator and the version can be apart.
                    if version.is_empty() {
                        version = tokens.next()?;
                    }
                    if !matches!(op, "" | "=" | ">" | ">=" | "<" | "<=" | "~" | "~>" | "^") {
                        return None;
                    }
                    interval = interval.intersect(comparator(op, Partial::parse(version)?));
                }
                interval
            }
        };
        if !interval.is_empty() {
            sets.push(interval);
        }
    }
    Some(sets)
}

/// Whether `node` satisfies `range`; `None` when the range can't be parsed.
pub fn satisfies(range: &str, node: Version) -> Option<bool> {
    Some(parse(range)?.iter().any(|i| i.contains(node)))
}

/// The oldest version newer than `node` that satisfies `range`, if any:
/// what to upgrade Node to (at least) for a package to run.
pub fn min_newer(range: &str, node: Version) -> Option<Version> {
    parse(range)?
        .iter()
        .filter(|i| i.low > node)
        .map(|i| i.low)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_common_engine_ranges() {
        let angular = "^22.22.3 || ^24.15.0 || >=26.0.0";
        assert_eq!(satisfies(angular, (24, 15, 0)), Some(true));
        assert_eq!(satisfies(angular, (24, 14, 9)), Some(false));
        assert_eq!(satisfies(angular, (23, 0, 0)), Some(false));
        assert_eq!(satisfies(angular, (26, 1, 0)), Some(true));
        assert_eq!(satisfies(angular, (12, 22, 12)), Some(false));

        assert_eq!(satisfies(">=14", (14, 0, 0)), Some(true));
        assert_eq!(satisfies(">= 14.17.0", (14, 16, 9)), Some(false));
        assert_eq!(satisfies(">14", (14, 9, 9)), Some(false));
        assert_eq!(satisfies(">14", (15, 0, 0)), Some(true));
        assert_eq!(satisfies("12.x", (12, 22, 12)), Some(true));
        assert_eq!(satisfies("12.x", (13, 0, 0)), Some(false));
        assert_eq!(satisfies("~12.20", (12, 21, 0)), Some(false));
        assert_eq!(satisfies("~12.20.1", (12, 20, 5)), Some(true));
        assert_eq!(satisfies("^0.10.0", (0, 11, 0)), Some(false));
        assert_eq!(satisfies("10 - 12", (12, 9, 0)), Some(true));
        assert_eq!(satisfies("10 - 12", (13, 0, 0)), Some(false));
        assert_eq!(satisfies(">=12 <16", (16, 0, 0)), Some(false));
        assert_eq!(satisfies(">=14.0.0-0", (14, 0, 0)), Some(true));
        assert_eq!(satisfies("*", (1, 0, 0)), Some(true));
        assert_eq!(satisfies("", (1, 0, 0)), Some(true));
        assert_eq!(satisfies("v18.0.0", (18, 0, 0)), Some(true));
        assert_eq!(satisfies("<=16.2", (16, 2, 9)), Some(true));
        assert_eq!(satisfies("<=16.2", (16, 3, 0)), Some(false));
    }

    #[test]
    fn unparseable_ranges_are_unknown() {
        assert_eq!(satisfies("latest", (20, 0, 0)), None);
        assert_eq!(satisfies(">=abc", (20, 0, 0)), None);
        assert_eq!(satisfies("!14", (20, 0, 0)), None);
        assert_eq!(satisfies(">=", (20, 0, 0)), None);
    }

    #[test]
    fn finds_the_minimum_node_to_upgrade_to() {
        let angular = "^22.22.3 || ^24.15.0 || >=26.0.0";
        assert_eq!(min_newer(angular, (12, 22, 12)), Some((22, 22, 3)));
        assert_eq!(min_newer(angular, (23, 0, 0)), Some((24, 15, 0)));
        assert_eq!(min_newer(angular, (24, 14, 0)), Some((24, 15, 0)));
        assert_eq!(
            min_newer("^18.19.1 || ^20.11.1 || >=22", (19, 0, 0)),
            Some((20, 11, 1))
        );
        // Only older Node versions satisfy it.
        assert_eq!(min_newer("<12", (14, 0, 0)), None);
    }
}
