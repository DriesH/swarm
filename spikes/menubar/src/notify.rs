//! Notification helpers GPUI does not have: authorization status, delivered
//! count, and the dock bounce. Posting itself goes through GPUI.

use block2::RcBlock;
use gpui::{App, SystemNotification, SystemNotificationAction};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSRequestUserAttentionType};
use objc2_foundation::{NSArray, NSBundle};
use objc2_user_notifications::{
    UNAuthorizationStatus, UNNotification, UNNotificationSettings, UNUserNotificationCenter,
};

pub const TAG: &str = "task-42";

pub fn in_bundle() -> bool {
    NSBundle::mainBundle().bundleIdentifier().is_some()
}

pub fn show(cx: &App, version: u32) {
    let (title, body) = match version {
        1 => ("Needs you", "Orchestrator asks: merge worker 2 into main?"),
        _ => (
            "Needs you (updated)",
            "Orchestrator asks again: merge worker 2?",
        ),
    };
    cx.show_system_notification(SystemNotification {
        tag: TAG.into(),
        title: title.into(),
        body: body.into(),
        actions: vec![
            SystemNotificationAction {
                id: "open".into(),
                label: "Open".into(),
            },
            SystemNotificationAction {
                id: "dismiss".into(),
                label: "Dismiss".into(),
            },
        ],
    });
    eprintln!("[notify] posted tag={TAG} version={version}");
}

/// Logs authorization status and how many notifications are in Notification Center.
/// Callbacks run on a background thread.
pub fn log_status(label: &'static str) {
    // `UNUserNotificationCenter` aborts the process outside an app bundle.
    if !in_bundle() {
        eprintln!("[notify:{label}] not in an app bundle; notifications are off");
        return;
    }
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let on_settings = RcBlock::new(move |settings: std::ptr::NonNull<UNNotificationSettings>| {
        // SAFETY: the framework passes a valid settings object for the call.
        let settings = unsafe { settings.as_ref() };
        let status = match settings.authorizationStatus() {
            UNAuthorizationStatus::NotDetermined => "not determined (prompt pending)",
            UNAuthorizationStatus::Denied => "denied",
            UNAuthorizationStatus::Authorized => "authorized",
            UNAuthorizationStatus::Provisional => "provisional",
            _ => "other",
        };
        eprintln!(
            "[notify:{label}] authorization: {status}, alert style: {:?}",
            settings.alertStyle()
        );
    });
    center.getNotificationSettingsWithCompletionHandler(&on_settings);

    let on_delivered = RcBlock::new(move |list: std::ptr::NonNull<NSArray<UNNotification>>| {
        // SAFETY: as above.
        let list = unsafe { list.as_ref() };
        let tags: Vec<String> = list
            .iter()
            .map(|n| n.request().identifier().to_string())
            .collect();
        eprintln!("[notify:{label}] delivered: {} {tags:?}", tags.len());
    });
    center.getDeliveredNotificationsWithCompletionHandler(&on_delivered);
}

/// Bounces the dock icon. macOS ignores it (returns 0) while the app is active,
/// and there is no dock icon in accessory mode.
pub fn bounce(mtm: MainThreadMarker, critical: bool) -> isize {
    let kind = if critical {
        NSRequestUserAttentionType::CriticalRequest
    } else {
        NSRequestUserAttentionType::InformationalRequest
    };
    NSApplication::sharedApplication(mtm).requestUserAttention(kind)
}

pub fn cancel_bounce(mtm: MainThreadMarker, request: isize) {
    NSApplication::sharedApplication(mtm).cancelUserAttentionRequest(request);
}

pub fn app_is_active(mtm: MainThreadMarker) -> bool {
    NSApplication::sharedApplication(mtm).isActive()
}

pub fn activation_policy(mtm: MainThreadMarker) -> isize {
    NSApplication::sharedApplication(mtm).activationPolicy().0
}
