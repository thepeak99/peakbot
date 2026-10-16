//! Versions (DESIGN §3.3).

use semver::Version;

/// Parse the first line of `<sidecar> --version` output: exactly
/// `peakbot <semver>` (trimmed).
pub fn parse_version_output(stdout: &str) -> Option<Version> {
    todo!()
}

/// The version of the installed service's binary. `Unknown` = exe
/// missing / command failed / unparseable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstalledVersion {
    Known(Version),
    Unknown,
}

/// A (installed, bundled) pair the user declined to replace. Only the
/// exact pair suppresses the dialog again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclinedReplace {
    pub installed: InstalledVersion,
    pub bundled: Version,
}

impl DeclinedReplace {
    /// `installed=<ver|unknown>\nbundled=<ver>\n`.
    pub fn render(&self) -> String {
        todo!()
    }

    pub fn parse(s: &str) -> Option<DeclinedReplace> {
        todo!()
    }

    /// Exact pair equality.
    pub fn matches(&self, installed: &InstalledVersion, bundled: &Version) -> bool {
        todo!()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        s.parse().unwrap()
    }

    // §9.1 case 4.
    #[test]
    fn t04_parse_version_output_accepts_the_locked_format() {
        assert_eq!(parse_version_output("peakbot 0.23.0\n"), Some(v("0.23.0")));
        let pre = parse_version_output("peakbot 0.24.0-rc.1").expect("pre-release parses");
        assert_eq!(pre, v("0.24.0-rc.1"));
        assert_eq!(
            parse_version_output("peakbot 0.23.0\nextra"),
            Some(v("0.23.0")),
            "only the first line counts"
        );
    }

    // §9.1 case 5.
    #[test]
    fn t05_parse_version_output_rejects_garbage() {
        for bad in ["", "peakbot", "peakbot x.y", "shifu 0.23.0", "0.23.0"] {
            assert_eq!(parse_version_output(bad), None, "{bad:?} must not parse");
        }
    }

    // §9.1 case 6.
    #[test]
    fn t06_declined_replace_roundtrip_and_exact_pair_match() {
        let known = DeclinedReplace {
            installed: InstalledVersion::Known(v("0.22.0")),
            bundled: v("0.23.0"),
        };
        let rt = DeclinedReplace::parse(&known.render()).expect("render→parse roundtrip");
        assert_eq!(rt, known);

        let unknown = DeclinedReplace {
            installed: InstalledVersion::Unknown,
            bundled: v("0.23.0"),
        };
        let rt2 = DeclinedReplace::parse(&unknown.render()).expect("unknown roundtrip");
        assert_eq!(rt2, unknown);

        assert_eq!(
            DeclinedReplace::parse("nonsense"),
            None,
            "garbage must not parse"
        );

        assert!(
            known.matches(&InstalledVersion::Known(v("0.22.0")), &v("0.23.0")),
            "exact pair must match"
        );
        assert!(
            !known.matches(&InstalledVersion::Known(v("0.22.0")), &v("0.24.0")),
            "different bundled must not match"
        );
        assert!(
            !known.matches(&InstalledVersion::Unknown, &v("0.23.0")),
            "Known vs Unknown must not match"
        );
    }
}
