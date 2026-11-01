//! What GPUI does not do for notifications: read the authorization and play a sound.
//!
//! The authorization answer comes on a thread of the system's choosing. It goes into a
//! channel and never touches GPUI state.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2_app_kit::NSSound;
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNNotificationSetting, UNNotificationSettings,
    UNUserNotificationCenter,
};

use super::{Authorization, Permission};

/// A short system sound.
const SOUND: &str = "Ping";

/// `UNUserNotificationCenter` aborts the process outside an app bundle, and only a bundle
/// has an identifier.
pub(super) fn read_permission(answer: flume::Sender<Authorization>) {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        send(&answer, Authorization::UNAVAILABLE);
        return;
    }

    let on_settings = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // SAFETY: the framework passes a valid settings object for the length of the call.
        let settings = unsafe { settings.as_ref() };

        send(
            &answer,
            authorization_of(settings.authorizationStatus(), settings.soundSetting()),
        );
    });

    UNUserNotificationCenter::currentNotificationCenter()
        .getNotificationSettingsWithCompletionHandler(&on_settings);
}

/// Shows the system's question when macOS has not asked yet, and answers with what the
/// settings say once it is over. The same bundle rule as [`read_permission`] holds.
pub(super) fn request_permission(answer: flume::Sender<Authorization>) {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        send(&answer, Authorization::UNAVAILABLE);
        return;
    }

    // The sound setting is part of the answer, so the settings are read again.
    let on_answer = RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
        read_permission(answer.clone());
    });

    UNUserNotificationCenter::currentNotificationCenter()
        .requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
            &on_answer,
        );
}

fn send(answer: &flume::Sender<Authorization>, authorization: Authorization) {
    if let Err(error) = answer.send(authorization) {
        log::debug!("nobody waits for the notification permission: {error}");
    }
}

pub(super) fn authorization_of(
    status: UNAuthorizationStatus,
    sound: UNNotificationSetting,
) -> Authorization {
    Authorization {
        permission: permission_of(status),
        sound: sound == UNNotificationSetting::Enabled,
    }
}

pub(super) fn permission_of(status: UNAuthorizationStatus) -> Permission {
    match status {
        UNAuthorizationStatus::NotDetermined => Permission::NotAsked,
        UNAuthorizationStatus::Denied => Permission::NotAllowed,
        UNAuthorizationStatus::Authorized
        | UNAuthorizationStatus::Provisional
        | UNAuthorizationStatus::Ephemeral => Permission::Allowed,
        _ => Permission::Unavailable,
    }
}

/// GPUI sets no sound on its notifications, so the sound setting plays one itself.
pub(super) fn play_sound() {
    let Some(sound) = NSSound::soundNamed(&NSString::from_str(SOUND)) else {
        log::debug!("the system has no sound named {SOUND}");
        return;
    };

    if !sound.play() {
        log::debug!("the notification sound did not start");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_authorization_status_maps_to_the_four_answers() {
        assert_eq!(
            permission_of(UNAuthorizationStatus::NotDetermined),
            Permission::NotAsked
        );
        assert_eq!(
            permission_of(UNAuthorizationStatus::Denied),
            Permission::NotAllowed
        );
        for status in [
            UNAuthorizationStatus::Authorized,
            UNAuthorizationStatus::Provisional,
            UNAuthorizationStatus::Ephemeral,
        ] {
            assert_eq!(permission_of(status), Permission::Allowed, "{status:?}");
        }
        assert_eq!(
            permission_of(UNAuthorizationStatus(99)),
            Permission::Unavailable
        );
    }

    #[test]
    fn outside_an_app_bundle_the_permission_is_unavailable() {
        let (answer, answers) = flume::bounded(1);

        read_permission(answer);

        assert_eq!(answers.try_recv(), Ok(Authorization::UNAVAILABLE));
    }

    #[test]
    fn outside_an_app_bundle_asking_for_the_permission_is_unavailable() {
        let (answer, answers) = flume::bounded(1);

        request_permission(answer);

        assert_eq!(answers.try_recv(), Ok(Authorization::UNAVAILABLE));
    }

    #[test]
    fn the_sound_setting_is_on_only_when_it_is_enabled() {
        let sound = |setting| authorization_of(UNAuthorizationStatus::Authorized, setting).sound;

        assert!(sound(UNNotificationSetting::Enabled));
        assert!(!sound(UNNotificationSetting::Disabled));
        assert!(!sound(UNNotificationSetting::NotSupported));
    }

    #[test]
    fn the_authorization_holds_both_the_permission_and_the_sound() {
        assert_eq!(
            authorization_of(
                UNAuthorizationStatus::Denied,
                UNNotificationSetting::Enabled
            ),
            Authorization {
                permission: Permission::NotAllowed,
                sound: true,
            }
        );
    }
}
