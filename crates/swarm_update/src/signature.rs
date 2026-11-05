//! Code signature checks with Security.framework (ADR 0010).
//!
//! The trust in an update comes from this check, not from the manifest: a build
//! is installed only when Apple's chain says that our team signed it.

use std::path::{Path, PathBuf};

use core_foundation::bundle::CFBundle;
use core_foundation::string::CFString;
use core_foundation::url::CFURL;
use security_framework::os::macos::code_signing::{Flags, SecCode, SecRequirement, SecStaticCode};

use crate::{Error, Result, Version};

/// Code identifier of `Swarm.app` (its bundle identifier).
pub(crate) const APP_IDENTIFIER: &str = "com.swarm.app";

/// The Developer ID team that signs Swarm.
const TEAM: &str = "P2DG27TUTZ";

/// The Developer ID requirement for code named `identifier` from `team`. The
/// same text as the daemon's check of the app.
fn requirement(identifier: &str, team: &str) -> String {
    format!(
        "anchor apple generic and identifier \"{identifier}\" \
         and certificate 1[field.1.2.840.113635.100.6.2.6] \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] \
         and certificate leaf[subject.OU] = \"{team}\""
    )
}

/// Checks a downloaded `Swarm.app` before it is kept.
pub trait SignatureVerifier {
    /// Fails unless `app` is `Swarm.app` of `version`, signed by our team.
    fn verify(&self, app: &Path, version: &Version) -> Result<()>;
}

/// The real check: Developer ID, team `P2DG27TUTZ`, identifier `com.swarm.app`,
/// every architecture and every nested binary, and the bundle version.
#[derive(Debug, Clone, Copy, Default)]
pub struct DeveloperIdVerifier;

impl SignatureVerifier for DeveloperIdVerifier {
    fn verify(&self, app: &Path, version: &Version) -> Result<()> {
        check_static_code(app, &requirement(APP_IDENTIFIER, TEAM))?;

        // After the signature check: the plist is sealed by then.
        check_bundle_version(app, version)
    }
}

fn signature_error(error: impl std::fmt::Display) -> Error {
    Error::Signature(error.to_string())
}

fn check_static_code(app: &Path, requirement: &str) -> Result<()> {
    let url = CFURL::from_path(app, true)
        .ok_or_else(|| signature_error("the path of the app is not valid"))?;
    let code = SecStaticCode::from_path(&url, Flags::NONE).map_err(signature_error)?;
    let requirement = requirement
        .parse::<SecRequirement>()
        .map_err(signature_error)?;

    code.check_validity(
        Flags::CHECK_ALL_ARCHITECTURES | Flags::STRICT_VALIDATE | Flags::CHECK_NESTED_CODE,
        &requirement,
    )
    .map_err(signature_error)
}

fn check_bundle_version(app: &Path, expected: &Version) -> Result<()> {
    let found = bundle_version(app)
        .ok_or_else(|| signature_error("the app has no CFBundleShortVersionString"))?;

    if found == expected.to_string() {
        Ok(())
    } else {
        Err(signature_error(format!(
            "the app is version {found}, the manifest says {expected}"
        )))
    }
}

fn bundle_version(app: &Path) -> Option<String> {
    bundle_string(app, "CFBundleShortVersionString")
}

/// A text value of the bundle's `Info.plist`.
pub(crate) fn bundle_string(app: &Path, key: &'static str) -> Option<String> {
    let bundle = CFBundle::new(CFURL::from_path(app, true)?)?;
    let key = CFString::from_static_string(key);
    let value = bundle
        .info_dictionary()
        .find(&key)?
        .downcast::<CFString>()?;

    Some(value.to_string())
}

/// The running app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningBuild {
    pub version: Version,
    /// The `.app` folder, when the process runs from a bundle.
    pub bundle: Option<PathBuf>,
    /// Its own code passes the Developer ID requirement. A `cargo run` or an
    /// ad hoc bundle does not, and the app does not update itself then.
    pub signed_by_team: bool,
}

pub fn running_build() -> RunningBuild {
    let bundle = std::env::current_exe()
        .ok()
        .and_then(|executable| bundle_of(&executable));
    let signed_by_team = bundle.is_some() && own_code_meets(&requirement(APP_IDENTIFIER, TEAM));

    RunningBuild {
        version: Version::current(),
        bundle,
        signed_by_team,
    }
}

/// `X.app` for `X.app/Contents/MacOS/<executable>`.
fn bundle_of(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;

    let in_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app";

    in_bundle.then(|| app.to_owned())
}

fn own_code_meets(requirement: &str) -> bool {
    let checked = SecCode::for_self(Flags::NONE).and_then(|code| {
        let requirement = requirement.parse::<SecRequirement>()?;

        code.check_validity(Flags::NONE, &requirement)
    });

    checked.is_ok()
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::test_support::TempFolder;

    /// A bundle around a copy of the test binary, signed ad hoc when `sign` is set.
    fn bundle(folder: &TempFolder, version: &str, sign: bool) -> PathBuf {
        let app = folder.path().join("Swarm.app");
        let macos = app.join("Contents/MacOS");
        std::fs::create_dir_all(&macos).expect("folders");

        std::fs::copy(
            std::env::current_exe().expect("exe"),
            macos.join("swarm-app"),
        )
        .expect("copy the test binary");
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.swarm.app</string>
<key>CFBundleExecutable</key><string>swarm-app</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
<key>CFBundleVersion</key><string>{version}</string>
</dict></plist>"#
            ),
        )
        .expect("plist");

        if sign {
            let status = Command::new("/usr/bin/codesign")
                .args(["--force", "--sign", "-"])
                .arg(&app)
                .output()
                .expect("codesign");
            assert!(status.status.success(), "{status:?}");
        }

        app
    }

    #[test]
    fn the_requirement_names_identifier_and_team() {
        assert_eq!(
            requirement("com.swarm.app", "P2DG27TUTZ"),
            "anchor apple generic and identifier \"com.swarm.app\" \
             and certificate 1[field.1.2.840.113635.100.6.2.6] \
             and certificate leaf[field.1.2.840.113635.100.6.1.13] \
             and certificate leaf[subject.OU] = \"P2DG27TUTZ\""
        );
    }

    #[test]
    fn the_real_requirement_is_for_our_team_and_app() {
        assert_eq!(APP_IDENTIFIER, "com.swarm.app");
        assert_eq!(TEAM, "P2DG27TUTZ");
    }

    #[test]
    fn the_verifier_refuses_an_ad_hoc_signed_bundle() {
        let folder = TempFolder::new("adhoc");
        let app = bundle(&folder, "1.2.0", true);

        let result = DeveloperIdVerifier.verify(&app, &Version::new(1, 2, 0));

        assert!(matches!(result, Err(Error::Signature(_))), "{result:?}");
    }

    #[test]
    fn the_verifier_refuses_an_unsigned_bundle() {
        let folder = TempFolder::new("unsigned");
        let app = bundle(&folder, "1.2.0", false);

        let result = DeveloperIdVerifier.verify(&app, &Version::new(1, 2, 0));

        assert!(matches!(result, Err(Error::Signature(_))), "{result:?}");
    }

    #[test]
    fn the_verifier_refuses_a_folder_that_is_not_an_app() {
        let folder = TempFolder::new("not-an-app");

        let result = DeveloperIdVerifier.verify(folder.path(), &Version::new(1, 2, 0));

        assert!(matches!(result, Err(Error::Signature(_))), "{result:?}");
    }

    #[test]
    fn the_static_check_passes_a_bundle_that_meets_the_requirement() {
        let folder = TempFolder::new("static-ok");
        let app = bundle(&folder, "1.2.0", true);

        let result = check_static_code(&app, "identifier \"com.swarm.app\"");

        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn the_static_check_refuses_a_bundle_that_misses_the_requirement() {
        let folder = TempFolder::new("static-no");
        let app = bundle(&folder, "1.2.0", true);

        let result = check_static_code(&app, "identifier \"com.other.app\"");

        assert!(matches!(result, Err(Error::Signature(_))), "{result:?}");
    }

    #[test]
    fn the_static_check_refuses_a_broken_requirement() {
        let folder = TempFolder::new("static-broken");
        let app = bundle(&folder, "1.2.0", true);

        assert!(check_static_code(&app, "this is not a requirement").is_err());
    }

    #[test]
    fn the_bundle_version_is_read_from_the_plist() {
        let folder = TempFolder::new("version");
        let app = bundle(&folder, "3.4.5", false);

        assert_eq!(bundle_version(&app).as_deref(), Some("3.4.5"));
    }

    #[test]
    fn the_bundle_identifier_is_read_from_the_plist() {
        let folder = TempFolder::new("identifier");
        let app = bundle(&folder, "3.4.5", false);

        assert_eq!(
            bundle_string(&app, "CFBundleIdentifier").as_deref(),
            Some(APP_IDENTIFIER)
        );
    }

    #[test]
    fn a_missing_key_is_none() {
        let folder = TempFolder::new("missing-key");
        let app = bundle(&folder, "3.4.5", false);

        assert_eq!(bundle_string(&app, "NoSuchKey"), None);
    }

    #[test]
    fn a_bundle_of_another_version_is_refused() {
        let folder = TempFolder::new("version-mismatch");
        let app = bundle(&folder, "1.1.0", false);

        let result = check_bundle_version(&app, &Version::new(1, 2, 0));

        assert!(matches!(result, Err(Error::Signature(_))), "{result:?}");
    }

    #[test]
    fn a_bundle_of_the_same_version_passes_the_version_check() {
        let folder = TempFolder::new("version-match");
        let app = bundle(&folder, "1.2.0", false);

        assert!(check_bundle_version(&app, &Version::new(1, 2, 0)).is_ok());
    }

    #[test]
    fn the_bundle_is_found_from_the_executable() {
        assert_eq!(
            bundle_of(Path::new(
                "/Applications/Swarm.app/Contents/MacOS/swarm-app"
            )),
            Some(PathBuf::from("/Applications/Swarm.app"))
        );
    }

    #[test]
    fn a_cargo_target_is_not_a_bundle() {
        assert_eq!(bundle_of(Path::new("/work/target/debug/swarm-app")), None);
        assert_eq!(
            bundle_of(Path::new("/x/Swarm.app/Contents/Other/swarm-app")),
            None
        );
        assert_eq!(
            bundle_of(Path::new("/x/Swarm/Contents/MacOS/swarm-app")),
            None
        );
    }

    #[test]
    fn the_test_binary_does_not_meet_the_team_requirement() {
        assert!(!own_code_meets(&requirement(APP_IDENTIFIER, TEAM)));
    }

    #[test]
    fn the_own_code_check_passes_a_requirement_that_it_meets() {
        let executable = std::env::current_exe().expect("exe");
        let identifier = executable.file_name().expect("name").to_string_lossy();

        assert!(own_code_meets(&format!("identifier \"{identifier}\"")));
    }

    #[test]
    fn a_test_run_is_not_a_signed_bundle() {
        let build = running_build();

        assert_eq!(build.bundle, None);
        assert!(!build.signed_by_team);
        assert_eq!(build.version, Version::current());
    }
}
