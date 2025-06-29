use crate::BUNDLE_IDENTIFIER;
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
                _center: &NSUserNotificationCenter,
                _notification: &NSUserNotification,
            ) {
                let _ = activate_application();
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

pub fn deliver(title: &str, message: &str) -> anyhow::Result<()> {
    #[expect(deprecated)]
    unsafe {
        let notification = NSUserNotification::new();
        notification.setTitle(Some(&NSString::from_str(title)));
        notification.setInformativeText(Some(&NSString::from_str(message)));
        NSUserNotificationCenter::defaultUserNotificationCenter()
            .deliverNotification(&notification);
    }
    Ok(())
}

fn swizzle_bundle_identifier() {
    define_class!(
        #[unsafe(super(NSObject))]
        #[ivars = ()]
        pub struct FakeBundle;

        impl FakeBundle {
            #[unsafe(method(bundleIdentifier))]
            fn bundle_identifier(&self) -> &NSString {
                ns_string!(BUNDLE_IDENTIFIER)
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
    unsafe {
        iter::successors(getppid_of(libc::getppid()), |&pid| getppid_of(pid))
            .find_map(|pid| NSRunningApplication::runningApplicationWithProcessIdentifier(pid))
            .map(|app| {
                app.activateWithOptions(
                    #[expect(deprecated)]
                    NSApplicationActivationOptions::ActivateIgnoringOtherApps,
                )
            })
    }
    .unwrap_or_default()
}

unsafe fn getppid_of(pid: libc::pid_t) -> Option<libc::pid_t> {
    if pid == 0 {
        return None;
    }

    #[repr(C)]
    struct proc_bsdshortinfo {
        pbsi_pid: u32,
        pbsi_ppid: u32,
        pbsi_pgid: u32,
        pbsi_status: u32,
        pbsi_comm: [u8; libc::MAXCOMLEN],
        pbsi_flags: u32,
        pbsi_uid: libc::uid_t,
        pbsi_gid: libc::gid_t,
        pbsi_ruid: libc::uid_t,
        pbsi_rgid: libc::gid_t,
        pbsi_svuid: libc::uid_t,
        pbsi_svgid: libc::gid_t,
        pbsi_rfu: u32,
    }
    const PROC_PIDT_SHORTBSDINFO: libc::c_int = 13;
    const PROC_PIDT_SHORTBSDINFO_SIZE: libc::c_int = size_of::<proc_bsdshortinfo>() as _;
    unsafe {
        let mut info = mem::zeroed::<proc_bsdshortinfo>();
        let ret = libc::proc_pidinfo(
            pid,
            PROC_PIDT_SHORTBSDINFO,
            0,
            &mut info as *mut _ as *mut _,
            PROC_PIDT_SHORTBSDINFO_SIZE,
        );
        (ret == PROC_PIDT_SHORTBSDINFO_SIZE).then_some(info.pbsi_ppid as libc::pid_t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn ancestor_pids() {
        let got = unsafe { iter::successors(Some(libc::getpid()), |&pid| getppid_of(pid)) }
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
