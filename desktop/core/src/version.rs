//! Versions (DESIGN §3.3).

use semver::Version;

/// Parse the first line of `<sidecar> --version` output: exactly
/// `peakbot <semver>` (trimmed).
pub fn parse_version_output(stdout: &str) -> Option<Version> {
    let first = stdout.lines().next()?.trim();
    let (name, rest) = first.split_once(' ')?;
    if name != "peakbot" {
        return None;
    }
    Version::parse(rest.trim()).ok()
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
}
