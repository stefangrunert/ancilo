//! Versions of a running daemon versus this program (app, CLI) – after an
//! update the daemon of the old version may still be running.

/// Version of the daemon's API contract. Raised when a client of the previous
/// protocol could no longer work with the daemon.
pub const PROTOCOL: u32 = 1;

/// What to do with a running daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compat {
    /// Same version: use it.
    Same,
    /// The daemon is older: stop it and start this version.
    Replace,
    /// The daemon is newer and speaks our protocol: use it (never downgrade).
    UseNewer,
    /// The daemon is newer and speaks another protocol: this program is too old.
    TooOld,
}

/// `1.2.3`, `1.2.3-beta.1` → comparable parts; pre-releases sort before the release.
fn parse(v: &str) -> (Vec<u64>, Option<String>) {
    let v = v.trim().trim_start_matches('v');
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (v, None),
    };
    let nums = core.split('.').map(|p| p.parse().unwrap_or(0)).collect();
    (nums, pre)
}

/// Orders two versions (semantic versioning, numeric parts, pre-release lower).
pub fn cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let ((mut na, pa), (mut nb, pb)) = (parse(a), parse(b));
    let len = na.len().max(nb.len());
    na.resize(len, 0);
    nb.resize(len, 0);
    na.cmp(&nb).then_with(|| match (pa, pb) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => {
            // Dot-separated identifiers; numeric ones compare as numbers.
            let (xs, ys): (Vec<&str>, Vec<&str>) = (x.split('.').collect(), y.split('.').collect());
            for (a, b) in xs.iter().zip(&ys) {
                let o = match (a.parse::<u64>(), b.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => a.cmp(b),
                };
                if o.is_ne() {
                    return o;
                }
            }
            xs.len().cmp(&ys.len())
        }
    })
}

/// Decides about a running daemon of version `running` (protocol `protocol`,
/// `None` for daemons from before the protocol was reported).
pub fn compat(own: &str, running: &str, protocol: Option<u32>) -> Compat {
    match cmp(running, own) {
        std::cmp::Ordering::Equal => Compat::Same,
        std::cmp::Ordering::Less => Compat::Replace,
        std::cmp::Ordering::Greater if protocol == Some(PROTOCOL) => Compat::UseNewer,
        std::cmp::Ordering::Greater => Compat::TooOld,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // covers: M9-AC-03
    #[test]
    fn an_older_daemon_is_replaced_a_newer_one_never_downgraded() {
        assert_eq!(compat("1.2.0", "1.2.0", Some(PROTOCOL)), Compat::Same);
        assert_eq!(compat("1.2.0", "1.1.9", Some(PROTOCOL)), Compat::Replace);
        assert_eq!(compat("1.2.0", "1.1.9", None), Compat::Replace);
        assert_eq!(compat("1.2.0", "1.10.0", Some(PROTOCOL)), Compat::UseNewer);
        assert_eq!(compat("1.2.0", "2.0.0", Some(PROTOCOL + 1)), Compat::TooOld);
        assert_eq!(compat("1.2.0", "1.3.0", None), Compat::TooOld);
        assert_eq!(
            compat("1.0.0", "1.0.0-beta.2", Some(PROTOCOL)),
            Compat::Replace
        );
        assert_eq!(
            compat("1.0.0-beta.2", "1.0.0-beta.10", Some(PROTOCOL)),
            Compat::UseNewer
        );
    }
}
