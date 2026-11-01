//! What GPUI does not do for notifications: read the authorization and play a sound.
//!
//! The authorization answer comes on a thread of the system's choosing. It goes into a
//! channel and never touches GPUI state.

use std::ptr::NonNull;

use block2::RcBlock;
use objc2_app_kit::NSSound;
use objc2_foundation::{NSBundle, NSString};
use objc2_user_notifications::{
    UNAuthorizationStatus, UNNotificationSettings, UNUserNotificationCenter,
};

use super::Permission;

/// A short system sound.
const SOUND: &str = "Ping";

/// `UNUserNotificationCenter` aborts the process outside an app bundle, and only a bundle
/// has an identifier.
pub(super) fn read_permission(answer: flume::Sender<Permission>) {
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        send(&answer, Permission::Unavailable);
        return;
    }

    let on_settings = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // SAFETY: the framework passes a valid settings object for the length of the call.
        let status = unsafe { settings.as_ref() }.authorizationStatus();

        send(&answer, permission_of(status));
    });

    UNUserNotificationCenter::currentNotificationCenter()
        .getNotificationSettingsWithCompletionHandler(&on_settings);
}

fn send(answer: &flume::Sender<Permission>, permission: Permission) {
    if let Err(error) = answer.send(permission) {
        log::debug!("nobody waits for the notification permission: {error}");
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

        assert_eq!(answers.try_recv(), Ok(Permission::Unavailable));
    }
}
