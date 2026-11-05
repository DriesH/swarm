//! The update manifest and the version rules (`docs/PLAN.md`, Updates).

use std::fmt;
use std::str::FromStr;

use serde::Deserialize;

use crate::{Error, Result};

/// A real manifest is a few hundred bytes. Anything bigger is not ours.
const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// A version with three numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// The version of this build: the workspace version.
    pub fn current() -> Self {
        env!("CARGO_PKG_VERSION").parse().unwrap_or_default()
    }

    /// A macOS version: `13.0` or `13.0.1`.
    fn parse_macos(text: &str) -> Option<Self> {
        let mut numbers = text.split('.').map(parse_number);

        let major = numbers.next()??;
        let minor = numbers.next()??;
        let patch = numbers.next().unwrap_or(Some(0))?;

        numbers
            .next()
            .is_none()
            .then_some(Self::new(major, minor, patch))
    }
}

/// Digits only, no sign, no leading zero.
fn parse_number(text: &str) -> Option<u32> {
    let plain = !text.is_empty()
        && text.bytes().all(|byte| byte.is_ascii_digit())
        && (text == "0" || !text.starts_with('0'));

    if plain { text.parse().ok() } else { None }
}

/// Why a text is not a `MAJOR.MINOR.PATCH` version.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0:?} is not a MAJOR.MINOR.PATCH version")]
pub struct VersionParseError(String);

impl FromStr for Version {
    type Err = VersionParseError;

    fn from_str(text: &str) -> std::result::Result<Self, Self::Err> {
        let mut numbers = text.split('.').map(parse_number);

        let parsed = (|| {
            let major = numbers.next()??;
            let minor = numbers.next()??;
            let patch = numbers.next()??;

            numbers
                .next()
                .is_none()
                .then_some(Self::new(major, minor, patch))
        })();

        parsed.ok_or_else(|| VersionParseError(text.to_owned()))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// The release the manifest points to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: Version,
    /// The zip of the notarized `Swarm.app`.
    pub url: String,
    /// Lowercase hex SHA-256 of the zip.
    pub sha256: String,
    pub minimum_macos: Version,
    pub published: String,
    pub notes_url: String,
}

/// The manifest as it is in the JSON file, before any check.
#[derive(Deserialize)]
struct RawManifest {
    version: String,
    url: String,
    sha256: String,
    minimum_macos: String,
    published: String,
    notes_url: String,
}

/// Parses the manifest. `prefix` is where `url` and `notes_url` must live
/// ([`crate::REPO_PREFIX`] in the app).
pub fn parse_manifest(bytes: &[u8], prefix: &str) -> Result<Manifest> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(field_error("manifest", "is too big"));
    }

    let raw: RawManifest = serde_json::from_slice(bytes).map_err(Error::ManifestJson)?;

    let version = raw
        .version
        .parse::<Version>()
        .map_err(|error| field_error("version", &error.to_string()))?;
    let minimum_macos = Version::parse_macos(&raw.minimum_macos)
        .ok_or_else(|| field_error("minimum_macos", "is not a macOS version"))?;

    if !is_sha256(&raw.sha256) {
        return Err(field_error("sha256", "is not 64 hex characters"));
    }
    check_url("url", &raw.url, prefix)?;
    check_url("notes_url", &raw.notes_url, prefix)?;

    Ok(Manifest {
        version,
        url: raw.url,
        sha256: raw.sha256.to_ascii_lowercase(),
        minimum_macos,
        published: raw.published,
        notes_url: raw.notes_url,
    })
}

fn field_error(field: &'static str, reason: &str) -> Error {
    Error::ManifestField {
        field,
        reason: reason.to_owned(),
    }
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The URL must live under `prefix`. The rest may not climb out of it or hide
/// a query, a fragment or control characters.
fn check_url(field: &'static str, url: &str, prefix: &str) -> Result<()> {
    let Some(rest) = url.strip_prefix(prefix).filter(|rest| !rest.is_empty()) else {
        return Err(field_error(field, "does not start with the release prefix"));
    };

    let clean = !rest.contains("..")
        && !rest.contains(['\\', '?', '#'])
        && !rest
            .chars()
            .any(|character| character.is_control() || character == ' ');
    if !clean {
        return Err(field_error(field, "is not a plain release URL"));
    }

    Ok(())
}

/// What the manifest means for the running build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Offer {
    /// The manifest is not newer than the running build.
    UpToDate,
    /// A newer build exists, but it needs a newer macOS.
    NeedsNewerMacos {
        needs: Version,
    },
    Available(Manifest),
}

/// Never offers a version that is not higher than `running`.
pub fn evaluate(manifest: &Manifest, running: &Version, macos: &Version) -> Offer {
    if manifest.version <= *running {
        return Offer::UpToDate;
    }

    if manifest.minimum_macos > *macos {
        return Offer::NeedsNewerMacos {
            needs: manifest.minimum_macos,
        };
    }

    Offer::Available(manifest.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PREFIX: &str = "https://example.test/swarm/";
    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn manifest_json(version: &str, hash: &str, url: &str, notes_url: &str) -> String {
        format!(
            r#"{{
                "version": "{version}",
                "url": "{url}",
                "sha256": "{hash}",
                "minimum_macos": "13.0",
                "published": "2026-12-01T10:00:00Z",
                "notes_url": "{notes_url}"
            }}"#
        )
    }

    fn parse(json: &str) -> Result<Manifest> {
        parse_manifest(json.as_bytes(), PREFIX)
    }

    fn good() -> String {
        manifest_json(
            "1.2.0",
            HASH,
            "https://example.test/swarm/releases/download/v1.2.0/Swarm-1.2.0.zip",
            "https://example.test/swarm/releases/tag/v1.2.0",
        )
    }

    fn manifest(version: &str, minimum_macos: &str) -> Manifest {
        Manifest {
            version: version.parse().expect("version"),
            url: format!("{PREFIX}Swarm-{version}.zip"),
            sha256: HASH.to_owned(),
            minimum_macos: Version::parse_macos(minimum_macos).expect("macos"),
            published: "2026-12-01T10:00:00Z".to_owned(),
            notes_url: format!("{PREFIX}tag/v{version}"),
        }
    }

    fn version(text: &str) -> Version {
        text.parse().expect("version")
    }

    #[test]
    fn a_good_manifest_is_parsed() {
        let parsed = parse(&good()).expect("manifest");

        assert_eq!(parsed.version, Version::new(1, 2, 0));
        assert_eq!(parsed.minimum_macos, Version::new(13, 0, 0));
        assert_eq!(parsed.sha256, HASH);
        assert_eq!(parsed.published, "2026-12-01T10:00:00Z");
    }

    #[test]
    fn the_real_manifest_shape_is_accepted_with_the_real_prefix() {
        let json = manifest_json(
            "1.2.0",
            HASH,
            "https://github.com/DriesH/swarm/releases/download/v1.2.0/Swarm-1.2.0.zip",
            "https://github.com/DriesH/swarm/releases/tag/v1.2.0",
        );

        assert!(parse_manifest(json.as_bytes(), crate::REPO_PREFIX).is_ok());
    }

    #[test]
    fn an_upper_case_hash_is_stored_in_lower_case() {
        let json = manifest_json(
            "1.2.0",
            &HASH.to_ascii_uppercase(),
            "https://example.test/swarm/a.zip",
            "https://example.test/swarm/n",
        );

        assert_eq!(parse(&json).expect("manifest").sha256, HASH);
    }

    #[test]
    fn bad_json_is_refused() {
        assert!(matches!(parse("{ nope"), Err(Error::ManifestJson(_))));
    }

    #[test]
    fn a_missing_field_is_refused() {
        let json = r#"{ "version": "1.2.0" }"#;

        assert!(matches!(parse(json), Err(Error::ManifestJson(_))));
    }

    #[test]
    fn a_version_that_is_not_three_numbers_is_refused() {
        for bad in [
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "v1.2.3",
            "1.2.3-beta",
            "01.2.3",
            "",
            "1..3",
        ] {
            let json = manifest_json(
                bad,
                HASH,
                "https://example.test/swarm/a.zip",
                "https://example.test/swarm/n",
            );

            assert!(
                matches!(
                    parse(&json),
                    Err(Error::ManifestField {
                        field: "version",
                        ..
                    })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_hash_that_is_not_64_hex_characters_is_refused() {
        for bad in [
            "",
            "abc",
            &HASH[..63],
            &format!("{HASH}0"),
            &HASH.replace('0', "g"),
        ] {
            let json = manifest_json(
                "1.2.0",
                bad,
                "https://example.test/swarm/a.zip",
                "https://example.test/swarm/n",
            );

            assert!(
                matches!(
                    parse(&json),
                    Err(Error::ManifestField {
                        field: "sha256",
                        ..
                    })
                ),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_url_outside_the_prefix_is_refused() {
        for bad in [
            "https://evil.test/swarm/a.zip",
            "http://example.test/swarm/a.zip",
            "https://example.test/swarm",
            "https://example.test/swarm-evil/a.zip",
            "https://example.test/swarm/",
            "",
        ] {
            let json = manifest_json("1.2.0", HASH, bad, "https://example.test/swarm/n");

            assert!(
                matches!(parse(&json), Err(Error::ManifestField { field: "url", .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_notes_url_outside_the_prefix_is_refused() {
        let json = manifest_json(
            "1.2.0",
            HASH,
            "https://example.test/swarm/a.zip",
            "https://evil.test/notes",
        );

        assert!(matches!(
            parse(&json),
            Err(Error::ManifestField {
                field: "notes_url",
                ..
            })
        ));
    }

    #[test]
    fn a_url_that_climbs_out_of_the_prefix_is_refused() {
        for bad in [
            "https://example.test/swarm/../evil/a.zip",
            "https://example.test/swarm/a.zip?x=1",
            "https://example.test/swarm/a.zip#x",
            "https://example.test/swarm/a b.zip",
            "https://example.test/swarm/a\\b.zip",
        ] {
            let json = manifest_json("1.2.0", HASH, bad, "https://example.test/swarm/n");

            assert!(parse(&json).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_huge_manifest_is_refused() {
        let json = format!("{}{}", good(), " ".repeat(MAX_MANIFEST_BYTES));

        assert!(parse(&json).is_err());
    }

    #[test]
    fn minimum_macos_may_have_two_or_three_numbers() {
        assert_eq!(Version::parse_macos("13.0"), Some(Version::new(13, 0, 0)));
        assert_eq!(Version::parse_macos("14.2.1"), Some(Version::new(14, 2, 1)));
        assert_eq!(Version::parse_macos("13"), None);
        assert_eq!(Version::parse_macos("13.0.1.2"), None);
        assert_eq!(Version::parse_macos("13.x"), None);
    }

    #[test]
    fn versions_order_by_number_not_by_text() {
        assert!(version("1.10.0") > version("1.9.9"));
        assert!(version("2.0.0") > version("1.99.99"));
        assert!(version("1.2.4") > version("1.2.3"));
        assert_eq!(version("1.2.3"), version("1.2.3"));
    }

    #[test]
    fn a_version_prints_as_it_was_parsed() {
        assert_eq!(version("10.20.30").to_string(), "10.20.30");
    }

    #[test]
    fn the_current_version_is_the_crate_version() {
        assert_eq!(Version::current().to_string(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn the_same_version_is_up_to_date() {
        let offer = evaluate(
            &manifest("1.2.0", "13.0"),
            &version("1.2.0"),
            &version("15.0.0"),
        );

        assert_eq!(offer, Offer::UpToDate);
    }

    #[test]
    fn a_lower_version_is_up_to_date() {
        let offer = evaluate(
            &manifest("1.1.9", "13.0"),
            &version("1.2.0"),
            &version("15.0.0"),
        );

        assert_eq!(offer, Offer::UpToDate);
    }

    #[test]
    fn a_higher_version_is_available() {
        let new = manifest("1.3.0", "13.0");

        let offer = evaluate(&new, &version("1.2.0"), &version("15.0.0"));

        assert_eq!(offer, Offer::Available(new));
    }

    #[test]
    fn a_higher_version_that_needs_a_newer_macos_says_which() {
        let offer = evaluate(
            &manifest("1.3.0", "15.0"),
            &version("1.2.0"),
            &version("14.6.1"),
        );

        assert_eq!(
            offer,
            Offer::NeedsNewerMacos {
                needs: Version::new(15, 0, 0)
            }
        );
    }

    #[test]
    fn the_exact_minimum_macos_is_enough() {
        let offer = evaluate(
            &manifest("1.3.0", "14.6"),
            &version("1.2.0"),
            &version("14.6.0"),
        );

        assert!(matches!(offer, Offer::Available(_)));
    }

    #[test]
    fn an_old_version_is_up_to_date_even_when_it_needs_a_newer_macos() {
        let offer = evaluate(
            &manifest("1.0.0", "99.0"),
            &version("1.2.0"),
            &version("14.0.0"),
        );

        assert_eq!(offer, Offer::UpToDate);
    }
}
