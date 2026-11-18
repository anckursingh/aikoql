//! Server paths named by a network caller must stay inside the server's own
//! directories.
//!
//! A local stdio process is its own trust boundary (it can already read what it
//! asks for), so it keeps naming any directory. A network caller, including every
//! tenant of a hosted instance, does not: the session forces `confine_paths`
//! (see `session.rs`) and these helpers refuse anything that resolves outside
//! the base directory, symlinks included.

//! **Residual, documented:** `within` checks a path and the caller then uses it, two
//! separate steps, so a concurrent process with write access to the base directory
//! could swap a path component for a symlink in between. Nothing in this engine
//! creates links (checked: no `symlink` or `hard_link` call outside tests), and in a
//! hosted pod the engine is the only writer, so the race is not reachable through the
//! API. A self-hosted operator must keep the memory and database directories
//! unwritable by other OS users. Race-proof opening (`openat2`, or the cap-std crate)
//! is the follow-up for deployments with another writer.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

/// Did the server mark this call as coming from the network?
pub(crate) fn is_confined(args: &crate::J) -> bool {
    args.get("confine_paths")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

fn outside(requested: &str) -> String {
    format!(
        "path '{}' is outside the directory this server serves",
        requested.escape_debug()
    )
}

/// `requested` resolved under `base` (a relative path is relative to `base`),
/// or an error if it would leave `base`.
pub(crate) fn within(base: &Path, requested: &str) -> Result<PathBuf, String> {
    if requested.contains('\0') {
        return Err(outside(requested));
    }
    let rp = Path::new(requested);
    let joined = if rp.is_absolute() {
        rp.to_path_buf()
    } else {
        base.join(rp)
    };
    // `..` is never needed by a caller and is the classic way out
    if joined
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(outside(requested));
    }
    if canonical(&joined).starts_with(canonical(base)) {
        Ok(joined)
    } else {
        Err(outside(requested))
    }
}

/// The path with the longest existing prefix canonicalized (so a symlink inside
/// the base that points outside is seen for what it is) and the rest appended
/// literally. The rest holds no `..`: callers check that first.
fn canonical(p: &Path) -> PathBuf {
    let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut existing = abs.as_path();
    let mut rest: Vec<&OsStr> = Vec::new();
    loop {
        if let Ok(mut c) = existing.canonicalize() {
            for part in rest.iter().rev() {
                c.push(part);
            }
            return c;
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent;
            }
            _ => return abs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aikoql-confine-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("base/sub")).unwrap();
        std::fs::create_dir_all(d.join("outside")).unwrap();
        d
    }

    #[test]
    fn paths_inside_the_base_are_accepted() {
        let d = scratch("in");
        let base = d.join("base");
        assert_eq!(within(&base, "sub").unwrap(), base.join("sub"));
        assert_eq!(
            within(&base, "not/yet/created").unwrap(),
            base.join("not/yet/created")
        );
        assert_eq!(
            within(&base, base.join("sub").to_str().unwrap()).unwrap(),
            base.join("sub")
        );
        assert!(within(&base, "").is_ok(), "the base itself");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn every_way_out_is_refused() {
        let d = scratch("out");
        let base = d.join("base");
        for bad in [
            "..",
            "../outside",
            "sub/../../outside",
            "sub/..",
            d.join("outside").to_str().unwrap(),
            "/etc",
            "/",
            "/tmp",
            "sub\0x",
        ] {
            assert!(within(&base, bad).is_err(), "{bad:?}");
        }
        // a sibling that shares the base's name as a prefix is not inside it
        std::fs::create_dir_all(d.join("base-evil")).unwrap();
        assert!(within(&base, d.join("base-evil").to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_base_cannot_lead_outside() {
        let d = scratch("link");
        let base = d.join("base");
        std::os::unix::fs::symlink(d.join("outside"), base.join("escape")).unwrap();
        assert!(within(&base, "escape").is_err());
        assert!(
            within(&base, "escape/new").is_err(),
            "a not-yet-existing child of the link"
        );
        // a symlink that stays inside is fine
        std::os::unix::fs::symlink(base.join("sub"), base.join("alias")).unwrap();
        assert!(within(&base, "alias").is_ok());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn the_flag_is_a_real_boolean() {
        assert!(is_confined(&serde_json::json!({"confine_paths": true})));
        for no in [
            serde_json::json!({}),
            serde_json::json!({"confine_paths": false}),
            serde_json::json!({"confine_paths": "true"}),
        ] {
            assert!(!is_confined(&no), "{no}");
        }
    }

    /// Seeded fuzz: whatever the request, an accepted path really lives under
    /// the base once symlinks are resolved, and anything containing `..` is
    /// refused.
    #[cfg(unix)]
    #[test]
    fn fuzz_an_accepted_path_is_always_inside_the_base() {
        let d = scratch("fuzz");
        let base = d.join("base");
        std::os::unix::fs::symlink(d.join("outside"), base.join("escape")).unwrap();
        let parts = [
            "a", "b", "sub", "escape", "..", ".", "", "outside", "base", "x y", "\u{e9}", "-",
        ];
        let mut x: u64 = 0xC0FF_EE00_1234_5678;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let (mut accepted, mut refused) = (0, 0);
        for _ in 0..20_000 {
            let n = 1 + (next() % 5) as usize;
            let mut req = String::new();
            if next() % 6 == 0 {
                req.push('/');
            }
            for i in 0..n {
                if i > 0 {
                    req.push('/');
                }
                req.push_str(parts[(next() % parts.len() as u64) as usize]);
            }
            match within(&base, &req) {
                Ok(p) => {
                    accepted += 1;
                    assert!(
                        !req.split('/').any(|c| c == ".."),
                        "{req:?} has .. and was accepted"
                    );
                    assert!(
                        canonical(&p).starts_with(canonical(&base)),
                        "{req:?} -> {p:?} escapes"
                    );
                }
                Err(_) => refused += 1,
            }
        }
        assert!(
            accepted > 100 && refused > 100,
            "the fuzz must reach both outcomes ({accepted}/{refused})"
        );
        let _ = std::fs::remove_dir_all(d);
    }
}
