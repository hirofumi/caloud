//! macOS notification system integration.
//!
//! This module provides desktop notification functionality for terminals that don't
//! support OSC 9 escape sequences (such as Apple Terminal). It uses the deprecated
//! NSUserNotificationCenter API and swizzles NSBundle's bundleIdentifier method
//! to masquerade as Terminal.app.
//!
//! When a notification is clicked, the host terminal application is automatically
//! activated using the functionality from the [`application`](super::application) module.

use super::application::{activate_host_application, find_host_application};
use anyhow::bail;
use objc2::ffi::{class_getInstanceMethod, method_exchangeImplementations};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{ClassType, MainThreadOnly, class, define_class, msg_send, sel};
use objc2_foundation::{
    MainThreadMarker, NSObjectProtocol, NSString, NSUserNotificationCenterDelegate, ns_string,
};
#[expect(deprecated)]
use objc2_foundation::{NSUserNotification, NSUserNotificationCenter};
use std::mem;
use std::sync::Once;

pub fn set_global_delegate() -> anyhow::Result<()> {
    let Some(main_thread_marker) = MainThreadMarker::new() else {
        bail!("must be called on the main thread");
    };

    define_class!(
        #[unsafe(super(NSObject))]
        #[thread_kind = MainThreadOnly]
        #[ivars = ()]
        pub struct NotificationDelegate;

        unsafe impl NSObjectProtocol for NotificationDelegate {}

        unsafe impl NSUserNotificationCenterDelegate for NotificationDelegate {
            #[expect(deprecated)]
            #[unsafe(method(userNotificationCenter:didActivateNotification:))]
            fn did_activate_notification(
                &self,
                center: &NSUserNotificationCenter,
                notification: &NSUserNotification,
            ) {
                let _ = activate_host_application();
                center.removeDeliveredNotification(notification);
            }
        }
    );

    static ONCE: Once = Once::new();
    unsafe {
        ONCE.call_once(|| {
            swizzle_bundle_identifier();
            let delegate: Retained<NotificationDelegate> = {
                let this = NotificationDelegate::alloc(main_thread_marker).set_ivars(());
                msg_send![super(this), init]
            };
            #[expect(deprecated)]
            NSUserNotificationCenter::defaultUserNotificationCenter()
                .setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            mem::forget(delegate);
        });
    }

    Ok(())
}

pub fn deliver_if_osc9_unsupported(title: &str, message: &str) -> anyhow::Result<bool> {
    if is_osc9_supported() {
        return Ok(false);
    }

    #[expect(deprecated)]
    {
        let notification = NSUserNotification::new();
        notification.setTitle(Some(&NSString::from_str(title)));
        notification.setInformativeText(Some(&NSString::from_str(message)));
        NSUserNotificationCenter::defaultUserNotificationCenter()
            .deliverNotification(&notification);
    }

    Ok(true)
}

fn swizzle_bundle_identifier() {
    define_class!(
        #[unsafe(super(NSObject))]
        #[ivars = ()]
        pub struct FakeBundle;

        impl FakeBundle {
            #[unsafe(method(bundleIdentifier))]
            fn bundle_identifier(&self) -> &NSString {
                ns_string!("com.apple.Terminal")
            }
        }
    );

    unsafe {
        method_exchangeImplementations(
            class_getInstanceMethod(class!(NSBundle), sel!(bundleIdentifier)) as *mut _,
            class_getInstanceMethod(FakeBundle::class(), sel!(bundleIdentifier)) as *mut _,
        );
    }
}

fn is_osc9_supported() -> bool {
    const GHOSTTY: &str = "com.mitchellh.ghostty"; // https://ghostty.org/docs/config/reference#desktop-notifications
    const ITERM2: &str = "com.googlecode.iterm2"; // https://iterm2.com/documentation-escape-codes.html

    find_host_application()
        .and_then(|app| app.bundleIdentifier())
        .map(|bundle_identifier| matches!(bundle_identifier.to_string().as_str(), GHOSTTY | ITERM2))
        .unwrap_or_default()
}
