//! Code signature check of the terminal socket peer (ADR 0008).

use std::ffi::c_void;
use std::os::fd::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::ptr;
use std::sync::OnceLock;

use core_foundation::base::{CFType, CFTypeRef, TCFType as _};
use core_foundation::data::CFData;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::{CFString, CFStringRef};
use security_framework::os::macos::code_signing::{
    Flags, GuestAttributes, SecCode, SecRequirement,
};

/// Code identifier of the app's main executable (its bundle identifier).
const APP_IDENTIFIER: &str = "com.swarm.app";

/// Code identifier the bundle script gives the daemon binary.
const DAEMON_IDENTIFIER: &str = "com.swarm.app.daemon";

/// `errSecCSReqFailed`: the code is valid but does not meet the requirement.
const REQUIREMENT_FAILED: i32 = -67050;

/// `errSecCSUnsigned`: the code has no signature.
const UNSIGNED: i32 = -67062;

/// `kSecCSSigningInformation`.
const SIGNING_INFORMATION: u32 = 1 << 1;

/// `kSecCodeSignatureAdhoc`: signed without a certificate.
const SIGNATURE_ADHOC: u32 = 0x2;

/// Size of an `audit_token_t`: eight `u32` values.
const AUDIT_TOKEN_BYTES: usize = 32;

/// The team the release build was made for, set by `script/bundle-mac`.
const BUILD_TEAM: Option<&str> = option_env!("SWARM_TEAM_ID");

/// The refusal for every peer when the daemon cannot trust its own signature.
const BROKEN_REFUSAL: &str = "the daemon cannot check its own signature";

/// The refusal for a peer that is not the signed app.
const NOT_THE_APP_REFUSAL: &str = "only the signed Swarm app can use the terminal socket";

/// The kernel's identity of the process behind a socket end.
pub(crate) type AuditToken = [u8; AUDIT_TOKEN_BYTES];

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecCodeCopyStaticCode(
        code: *const c_void,
        flags: u32,
        static_code: *mut *const c_void,
    ) -> i32;

    fn SecCodeCopySigningInformation(
        static_code: *const c_void,
        flags: u32,
        information: *mut CFDictionaryRef,
    ) -> i32;

    static kSecCodeInfoTeamIdentifier: CFStringRef;
    static kSecCodeInfoIdentifier: CFStringRef;
    static kSecCodeInfoFlags: CFStringRef;
}

/// How the running daemon is signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonSigning {
    /// Signed by a Developer ID team.
    Developer { team: String },
    /// Ad hoc signed or not signed at all: a `cargo` build, an ad hoc bundle or a test.
    Unsigned,
    /// The daemon could not prove either state. Every peer is refused.
    Broken { reason: String },
}

/// What the daemon's own code signature showed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnSignature {
    Unsigned,
    DeveloperId { team: String },
    Unknown { reason: String },
}

/// The parts of the daemon's own signing information that matter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct OwnInfo {
    identifier: Option<String>,
    flags: Option<u32>,
    team: Option<String>,
}

/// Why the daemon's own signing information could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OwnInfoError {
    Unsigned,
    Other(String),
}

/// What checking the peer's code signature gave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PeerCheck {
    Valid,
    Invalid,
    Failed,
}

/// The Developer ID requirement the code named `identifier` must meet to be from `team`.
pub(crate) fn requirement(identifier: &str, team: &str) -> String {
    format!(
        "anchor apple generic and identifier \"{identifier}\" \
         and certificate 1[field.1.2.840.113635.100.6.2.6] \
         and certificate leaf[field.1.2.840.113635.100.6.1.13] \
         and certificate leaf[subject.OU] = \"{team}\""
    )
}

/// Why the peer must not talk to the daemon, if it must not. The peer is only
/// checked when the daemon itself is Developer ID signed.
pub(crate) fn signature_refusal(
    signing: &DaemonSigning,
    check_peer: impl FnOnce(&str) -> PeerCheck,
) -> Option<String> {
    let team = match signing {
        DaemonSigning::Unsigned => return None,
        DaemonSigning::Broken { .. } => return Some(BROKEN_REFUSAL.to_owned()),
        DaemonSigning::Developer { team } => team,
    };

    match check_peer(team) {
        PeerCheck::Valid => None,
        PeerCheck::Invalid => Some(NOT_THE_APP_REFUSAL.to_owned()),
        PeerCheck::Failed => Some("cannot check the connecting process".to_owned()),
    }
}

/// Signature refusal for the process behind `token`.
pub(crate) fn refusal(token: &AuditToken) -> Option<String> {
    signature_refusal(daemon_signing(), |team| {
        check_peer(token, &requirement(APP_IDENTIFIER, team))
    })
}

/// Read once; logs once what it found. `main` reads it at start, before an update can
/// replace the binary on disk.
pub(crate) fn daemon_signing() -> &'static DaemonSigning {
    static SIGNING: OnceLock<DaemonSigning> = OnceLock::new();

    SIGNING.get_or_init(|| {
        let signing = if cfg!(test) {
            DaemonSigning::Unsigned
        } else {
            detect_signing(BUILD_TEAM)
        };

        match &signing {
            DaemonSigning::Unsigned => {
                log::info!("daemon is not Developer ID signed, skipping the peer signature check");
            }
            DaemonSigning::Broken { reason } => {
                log::error!("daemon cannot check its own signature: {reason}");
            }
            DaemonSigning::Developer { team } => {
                log::info!("daemon is Developer ID signed for team {team}");
            }
        }

        signing
    })
}

/// The daemon's signing state. `build_team` is the team a release build was made for.
fn detect_signing(build_team: Option<&str>) -> DaemonSigning {
    let evidence = match read_own_info() {
        Ok(info) => classify(&info, own_meets_developer_id),
        Err(OwnInfoError::Unsigned) => OwnSignature::Unsigned,
        Err(OwnInfoError::Other(reason)) => OwnSignature::Unknown { reason },
    };

    decide_signing(build_team, evidence)
}

/// Only positive evidence turns the check off. Everything unclear is `Broken`.
fn decide_signing(build_team: Option<&str>, evidence: OwnSignature) -> DaemonSigning {
    match (build_team, evidence) {
        (Some(expected), OwnSignature::DeveloperId { team }) if team == expected => {
            DaemonSigning::Developer { team }
        }
        (Some(expected), OwnSignature::DeveloperId { team }) => DaemonSigning::Broken {
            reason: format!("signed by team {team}, built for team {expected}"),
        },
        (Some(expected), OwnSignature::Unsigned) => DaemonSigning::Broken {
            reason: format!("built for team {expected} but not Developer ID signed"),
        },
        (None, OwnSignature::DeveloperId { team }) => DaemonSigning::Developer { team },
        (None, OwnSignature::Unsigned) => DaemonSigning::Unsigned,
        (_, OwnSignature::Unknown { reason }) => DaemonSigning::Broken { reason },
    }
}

/// `meets_developer_id` checks the daemon against the Developer ID requirement for a team.
fn classify(
    info: &OwnInfo,
    meets_developer_id: impl FnOnce(&str) -> Result<(), String>,
) -> OwnSignature {
    if info.identifier.is_none() {
        return OwnSignature::Unsigned;
    }

    let Some(flags) = info.flags else {
        return OwnSignature::Unknown {
            reason: "cannot read the code signing flags".to_owned(),
        };
    };
    if flags & SIGNATURE_ADHOC != 0 {
        return OwnSignature::Unsigned;
    }

    let Some(team) = info.team.as_deref().filter(|team| is_team_identifier(team)) else {
        return OwnSignature::Unknown {
            reason: "the signature has no valid team identifier".to_owned(),
        };
    };

    match meets_developer_id(team) {
        Ok(()) => OwnSignature::DeveloperId {
            team: team.to_owned(),
        },
        Err(reason) => OwnSignature::Unknown { reason },
    }
}

/// Team identifiers are ten ASCII uppercase letters and digits. Anything else must never
/// reach a requirement string.
fn is_team_identifier(team: &str) -> bool {
    team.len() == 10
        && team
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
}

fn own_meets_developer_id(team: &str) -> Result<(), String> {
    let code = SecCode::for_self(Flags::NONE).map_err(|error| error.to_string())?;
    let requirement = requirement(DAEMON_IDENTIFIER, team)
        .parse::<SecRequirement>()
        .map_err(|error| error.to_string())?;

    code.check_validity(Flags::NONE, &requirement)
        .map_err(|error| error.to_string())
}

fn status_error(what: &str, status: i32) -> OwnInfoError {
    if status == UNSIGNED {
        OwnInfoError::Unsigned
    } else {
        OwnInfoError::Other(format!("{what} failed with status {status}"))
    }
}

fn read_own_info() -> Result<OwnInfo, OwnInfoError> {
    let code = SecCode::for_self(Flags::NONE).map_err(|error| {
        if error.code() == UNSIGNED {
            OwnInfoError::Unsigned
        } else {
            OwnInfoError::Other(error.to_string())
        }
    })?;

    let mut static_code: *const c_void = ptr::null();
    // SAFETY: `code` is a live SecCode and `static_code` is a valid out pointer.
    let status = unsafe {
        SecCodeCopyStaticCode(code.as_concrete_TypeRef().cast(), 0, &raw mut static_code)
    };
    if status != 0 || static_code.is_null() {
        return Err(status_error("SecCodeCopyStaticCode", status));
    }
    // SAFETY: the call returned an owned (+1) reference.
    let static_code = unsafe { CFType::wrap_under_create_rule(static_code as CFTypeRef) };

    let mut information: CFDictionaryRef = ptr::null();
    // SAFETY: `static_code` is a live SecStaticCode and `information` is a valid out pointer.
    let status = unsafe {
        SecCodeCopySigningInformation(
            static_code.as_concrete_TypeRef().cast(),
            SIGNING_INFORMATION,
            &raw mut information,
        )
    };
    if status != 0 || information.is_null() {
        return Err(status_error("SecCodeCopySigningInformation", status));
    }
    // SAFETY: the call returned an owned (+1) dictionary.
    let information: CFDictionary<*const c_void, *const c_void> =
        unsafe { CFDictionary::wrap_under_create_rule(information) };

    // SAFETY: the keys are constants that live as long as the process.
    let (identifier_key, flags_key, team_key) = unsafe {
        (
            kSecCodeInfoIdentifier,
            kSecCodeInfoFlags,
            kSecCodeInfoTeamIdentifier,
        )
    };

    Ok(OwnInfo {
        identifier: info_value(&information, identifier_key)
            .and_then(|value| value.downcast::<CFString>())
            .map(|identifier| identifier.to_string()),
        flags: info_value(&information, flags_key)
            .and_then(|value| value.downcast::<CFNumber>())
            .and_then(|flags| flags.to_i64())
            .and_then(|flags| u32::try_from(flags).ok()),
        team: info_value(&information, team_key)
            .and_then(|value| value.downcast::<CFString>())
            .map(|team| team.to_string()),
    })
}

fn info_value(
    information: &CFDictionary<*const c_void, *const c_void>,
    key: CFStringRef,
) -> Option<CFType> {
    // SAFETY: the key is a constant that lives as long as the process.
    let key = unsafe { CFString::wrap_under_get_rule(key) };
    let value = information.find(key.as_concrete_TypeRef().cast::<c_void>())?;

    // SAFETY: the dictionary owns the value and outlives this borrow.
    Some(unsafe { CFType::wrap_under_get_rule(*value as CFTypeRef) })
}

fn check_peer(token: &AuditToken, requirement: &str) -> PeerCheck {
    match peer_code(token).and_then(|code| {
        let requirement = requirement.parse::<SecRequirement>()?;
        code.check_validity(Flags::NONE, &requirement)
    }) {
        Ok(()) => PeerCheck::Valid,
        Err(error) if error.code() == REQUIREMENT_FAILED => PeerCheck::Invalid,
        Err(error) => {
            log::warn!("cannot check the signature of the connecting process: {error}");
            PeerCheck::Failed
        }
    }
}

/// The code of the process behind `token`. The token names the exact process, so a reused
/// pid cannot fool it. On its own it does not prove who talks later: `exec` changes what
/// the socket's token says. The daemon pins the token and runs a challenge for that.
fn peer_code(token: &AuditToken) -> Result<SecCode, security_framework::base::Error> {
    let token = CFData::from_buffer(token);

    let mut attributes = GuestAttributes::new();
    attributes.set_audit_token(token.as_concrete_TypeRef());

    SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE)
}

/// The audit token of the process on the other end of `stream`, as of now.
pub(crate) fn peer_audit_token(stream: &UnixStream) -> Option<AuditToken> {
    let mut token = [0_u8; AUDIT_TOKEN_BYTES];
    let mut length = libc::socklen_t::try_from(AUDIT_TOKEN_BYTES).ok()?;

    // SAFETY: the value pointer is valid for `length` bytes and both outlive the call.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            token.as_mut_ptr().cast(),
            &raw mut length,
        )
    };

    (result == 0 && usize::try_from(length) == Ok(AUDIT_TOKEN_BYTES)).then_some(token)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::os::unix::net::UnixListener;

    use super::*;
    use crate::temp_folder::TempFolder;

    /// A connected pair. A `UnixStream::pair()` has no peer token (ENOTCONN), so connect for real.
    /// Returns the client and the accepted side, whose peer is this process.
    fn connected_pair() -> (UnixStream, UnixStream) {
        let folder = TempFolder::new("peer");
        let path = folder.path().join("s");

        let listener = UnixListener::bind(&path).expect("bind");
        let client = UnixStream::connect(&path).expect("connect");
        let (accepted, _) = listener.accept().expect("accept");

        (client, accepted)
    }

    fn developer() -> DaemonSigning {
        DaemonSigning::Developer {
            team: "TEAM123456".to_owned(),
        }
    }

    fn unknown() -> OwnSignature {
        OwnSignature::Unknown {
            reason: "x".to_owned(),
        }
    }

    fn developer_id(team: &str) -> OwnSignature {
        OwnSignature::DeveloperId {
            team: team.to_owned(),
        }
    }

    fn info(identifier: Option<&str>, flags: Option<u32>, team: Option<&str>) -> OwnInfo {
        OwnInfo {
            identifier: identifier.map(str::to_owned),
            flags,
            team: team.map(str::to_owned),
        }
    }

    #[test]
    fn signed_daemon_accepts_a_valid_peer() {
        assert_eq!(signature_refusal(&developer(), |_| PeerCheck::Valid), None);
    }

    #[test]
    fn signed_daemon_refuses_an_invalid_peer() {
        assert_eq!(
            signature_refusal(&developer(), |_| PeerCheck::Invalid).as_deref(),
            Some(NOT_THE_APP_REFUSAL)
        );
    }

    #[test]
    fn signed_daemon_refuses_when_the_check_fails() {
        assert!(signature_refusal(&developer(), |_| PeerCheck::Failed).is_some());
    }

    #[test]
    fn signed_daemon_checks_the_peer_against_its_own_team() {
        let seen = Cell::new(String::new());

        signature_refusal(&developer(), |team| {
            seen.set(team.to_owned());
            PeerCheck::Valid
        });

        assert_eq!(seen.into_inner(), "TEAM123456");
    }

    #[test]
    fn unsigned_daemon_accepts_any_peer_without_checking() {
        let checked = Cell::new(false);

        let refusal = signature_refusal(&DaemonSigning::Unsigned, |_| {
            checked.set(true);
            PeerCheck::Invalid
        });

        assert_eq!((refusal, checked.get()), (None, false));
    }

    #[test]
    fn broken_daemon_refuses_every_peer_without_checking() {
        let checked = Cell::new(false);
        let broken = DaemonSigning::Broken {
            reason: "x".to_owned(),
        };

        let refusal = signature_refusal(&broken, |_| {
            checked.set(true);
            PeerCheck::Valid
        });

        assert_eq!(
            (refusal.as_deref(), checked.get()),
            (Some(BROKEN_REFUSAL), false)
        );
    }

    #[test]
    fn requirement_names_identifier_and_team() {
        assert_eq!(
            requirement("com.swarm.app", "P2DG27TUTZ"),
            "anchor apple generic and identifier \"com.swarm.app\" \
             and certificate 1[field.1.2.840.113635.100.6.2.6] \
             and certificate leaf[field.1.2.840.113635.100.6.1.13] \
             and certificate leaf[subject.OU] = \"P2DG27TUTZ\""
        );
    }

    #[test]
    fn team_identifier_is_ten_uppercase_letters_and_digits() {
        assert!(is_team_identifier("P2DG27TUTZ"));
    }

    #[test]
    fn team_identifier_rejects_quotes_and_empty() {
        assert!(!is_team_identifier("A\" or anchor apple"));
        assert!(!is_team_identifier(""));
    }

    #[test]
    fn team_identifier_rejects_other_lengths() {
        assert!(!is_team_identifier("P2DG27TUT"));
        assert!(!is_team_identifier("P2DG27TUTZZ"));
    }

    #[test]
    fn team_identifier_rejects_lowercase() {
        assert!(!is_team_identifier("p2dg27tutz"));
    }

    #[test]
    fn team_identifier_rejects_non_ascii() {
        assert!(!is_team_identifier("P2DG27TUTÄ"));
    }

    #[test]
    fn code_without_an_identifier_is_unsigned() {
        let result = classify(&info(None, None, None), |_| Ok(()));

        assert_eq!(result, OwnSignature::Unsigned);
    }

    #[test]
    fn ad_hoc_code_is_unsigned() {
        let result = classify(&info(Some("x"), Some(0x2_0002), None), |_| Ok(()));

        assert_eq!(result, OwnSignature::Unsigned);
    }

    #[test]
    fn code_that_meets_developer_id_gives_its_team() {
        let result = classify(&info(Some("x"), Some(0x10000), Some("P2DG27TUTZ")), |_| {
            Ok(())
        });

        assert_eq!(result, developer_id("P2DG27TUTZ"));
    }

    #[test]
    fn code_that_fails_developer_id_is_unknown_not_unsigned() {
        let result = classify(&info(Some("x"), Some(0), Some("P2DG27TUTZ")), |_| {
            Err("failed".to_owned())
        });

        assert!(matches!(result, OwnSignature::Unknown { .. }));
    }

    #[test]
    fn unreadable_flags_are_unknown() {
        let result = classify(&info(Some("x"), None, Some("P2DG27TUTZ")), |_| Ok(()));

        assert!(matches!(result, OwnSignature::Unknown { .. }));
    }

    #[test]
    fn a_signature_without_a_valid_team_is_unknown() {
        let missing = classify(&info(Some("x"), Some(0), None), |_| Ok(()));
        let invalid = classify(&info(Some("x"), Some(0), Some("bad")), |_| Ok(()));

        assert!(matches!(missing, OwnSignature::Unknown { .. }));
        assert!(matches!(invalid, OwnSignature::Unknown { .. }));
    }

    #[test]
    fn without_a_build_team_unsigned_stays_unsigned() {
        assert_eq!(
            decide_signing(None, OwnSignature::Unsigned),
            DaemonSigning::Unsigned
        );
    }

    #[test]
    fn without_a_build_team_a_developer_id_signature_is_trusted() {
        assert_eq!(
            decide_signing(None, developer_id("P2DG27TUTZ")),
            DaemonSigning::Developer {
                team: "P2DG27TUTZ".to_owned()
            }
        );
    }

    #[test]
    fn an_error_is_never_unsigned() {
        for build_team in [None, Some("P2DG27TUTZ")] {
            assert!(matches!(
                decide_signing(build_team, unknown()),
                DaemonSigning::Broken { .. }
            ));
        }
    }

    #[test]
    fn a_build_team_accepts_only_its_own_signature() {
        assert_eq!(
            decide_signing(Some("P2DG27TUTZ"), developer_id("P2DG27TUTZ")),
            DaemonSigning::Developer {
                team: "P2DG27TUTZ".to_owned()
            }
        );
    }

    #[test]
    fn a_build_team_rejects_another_team() {
        assert!(matches!(
            decide_signing(Some("P2DG27TUTZ"), developer_id("OTHER12345")),
            DaemonSigning::Broken { .. }
        ));
    }

    #[test]
    fn a_build_team_rejects_an_unsigned_daemon() {
        assert!(matches!(
            decide_signing(Some("P2DG27TUTZ"), OwnSignature::Unsigned),
            DaemonSigning::Broken { .. }
        ));
    }

    #[test]
    fn tests_run_as_an_unsigned_daemon() {
        assert_eq!(daemon_signing(), &DaemonSigning::Unsigned);
    }

    #[test]
    fn an_ad_hoc_test_binary_is_detected_as_unsigned() {
        assert_eq!(detect_signing(None), DaemonSigning::Unsigned);
    }

    #[test]
    fn an_ad_hoc_test_binary_is_broken_for_a_release_team() {
        assert!(matches!(
            detect_signing(Some("P2DG27TUTZ")),
            DaemonSigning::Broken { .. }
        ));
    }

    #[test]
    fn audit_token_of_a_socket_pair_is_this_process() {
        let (_client, peer) = connected_pair();

        let token = peer_audit_token(&peer).expect("token");

        // Index 5 of an audit token is the pid.
        let pid = u32::from_ne_bytes([token[20], token[21], token[22], token[23]]);
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn peer_code_of_a_socket_pair_is_built() {
        let (_client, peer) = connected_pair();
        let token = peer_audit_token(&peer).expect("token");

        assert!(peer_code(&token).is_ok());
    }

    #[test]
    fn peer_that_is_not_the_app_is_invalid() {
        let (_client, peer) = connected_pair();
        let token = peer_audit_token(&peer).expect("token");

        let check = check_peer(&token, &requirement(APP_IDENTIFIER, "P2DG27TUTZ"));

        assert_eq!(check, PeerCheck::Invalid);
    }

    #[test]
    fn peer_that_meets_the_requirement_is_valid() {
        let (_client, peer) = connected_pair();
        let token = peer_audit_token(&peer).expect("token");
        let exe = std::env::current_exe().expect("exe");
        let identifier = exe
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();

        let check = check_peer(&token, &format!("identifier \"{identifier}\""));

        assert_eq!(check, PeerCheck::Valid);
    }

    #[test]
    fn broken_requirement_fails_the_check() {
        let (_client, peer) = connected_pair();
        let token = peer_audit_token(&peer).expect("token");

        assert_eq!(
            check_peer(&token, "this is not a requirement"),
            PeerCheck::Failed
        );
    }
}
