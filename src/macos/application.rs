//! macOS application discovery utilities.
//!
//! This module provides functionality to locate and activate the terminal application
//! that hosts the current process by traversing the process tree.

use crate::macos::sys_proc_info::{PROC_PIDT_SHORTBSDINFO, proc_bsdshortinfo};
use nix::libc;
use objc2::rc::Retained;
use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
use std::{iter, mem};

pub fn activate_host_application() -> bool {
    find_host_application()
        .map(|app| {
            app.activateWithOptions(
                #[expect(deprecated)]
                NSApplicationActivationOptions::ActivateIgnoringOtherApps,
            )
        })
        .unwrap_or_default()
}

pub fn find_host_application() -> Option<Retained<NSRunningApplication>> {
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
