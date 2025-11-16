use crate::macos::sys_proc_info::{PROC_PIDT_SHORTBSDINFO, proc_bsdshortinfo};
use anyhow::bail;
use nix::libc;
use objc2::ffi::{class_getInstanceMethod, method_exchangeImplementations};
use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{ClassType, MainThreadOnly, class, define_class, msg_send, sel};
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use objc2_foundation::{
    MainThreadMarker, NSObjectProtocol, NSString, NSUserNotificationCenterDelegate, ns_string,
};
#[expect(deprecated)]
use objc2_foundation::{NSUserNotification, NSUserNotificationCenter};
use std::iter;
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
                let _ = activate_application();
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

fn activate_application() -> bool {
    find_application()
        .map(|app| {
            app.activateWithOptions(
                #[expect(deprecated)]
                NSApplicationActivationOptions::ActivateIgnoringOtherApps,
            )
        })
        .unwrap_or_default()
}

fn is_osc9_supported() -> bool {
    const GHOSTTY: &str = "com.mitchellh.ghostty"; // https://ghostty.org/docs/config/reference#desktop-notifications
    const ITERM2: &str = "com.googlecode.iterm2"; // https://iterm2.com/documentation-escape-codes.html

    find_application()
        .and_then(|app| app.bundleIdentifier())
        .map(|bundle_identifier| matches!(bundle_identifier.to_string().as_str(), GHOSTTY | ITERM2))
        .unwrap_or_default()
}

fn find_application() -> Option<Retained<NSRunningApplication>> {
    iterate_ancestor_pids().find_map(|pid| {
        NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
            .filter(|app| app.bundleIdentifier().is_some())
    })
}

fn iterate_ancestor_pids() -> impl Iterator<Item = libc::pid_t> {
    iter::successors(Some(std::process::id() as _), |&pid| getppid_of(pid))
}

fn getppid_of(pid: libc::pid_t) -> Option<libc::pid_t> {
    if pid == 0 {
        return None;
    }

    unsafe {
        let mut info = mem::zeroed::<proc_bsdshortinfo>();
        let ret = libc::proc_pidinfo(
            pid,
            PROC_PIDT_SHORTBSDINFO as libc::c_int,
            0,
            &mut info as *mut _ as *mut _,
            size_of::<proc_bsdshortinfo>() as libc::c_int,
        );
        (ret == size_of::<proc_bsdshortinfo>() as _).then_some(info.pbsi_ppid as libc::pid_t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn ancestor_pids() {
        let got = iterate_ancestor_pids()
            .map(|pid| pid as u32)
            .collect::<Vec<_>>();
        let want = iter::successors(Some(std::process::id()), |pid| {
            Command::new("ps")
                .args(["-p", &pid.to_string(), "-o", "ppid="])
                .output()
                .ok()
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .and_then(|stdout| stdout.trim().parse().ok())
        })
        .collect::<Vec<_>>();
        assert_eq!(got, want);
    }
}
