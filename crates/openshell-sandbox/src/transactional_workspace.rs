// SPDX-FileCopyrightText: Copyright (c) 2025-2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Experimental transactional workspace support.
//!
//! Supports Linux-only transactional workdirs backed by either kernel OverlayFS
//! or `fuse-overlayfs`. The rootless path is intended to use FUSE.

use crate::policy::SandboxPolicy;
use miette::{IntoDiagnostic, Result, WrapErr};
use std::fs;
#[cfg(target_os = "linux")]
use std::io::ErrorKind;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionalWorkspaceBackend {
    Off,
    Auto,
    KernelOverlay,
    FuseOverlay,
}

impl TransactionalWorkspaceBackend {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "off" | "none" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            "kernel" | "kernel-overlay" | "overlayfs" => Ok(Self::KernelOverlay),
            "fuse" | "fuse-overlay" | "fuse-overlayfs" => Ok(Self::FuseOverlay),
            other => Err(miette::miette!(
                "Unknown transactional workspace backend '{other}'. Expected one of: off, auto, kernel, fuse"
            )),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TransactionalWorkspaceConfig {
    pub root: PathBuf,
    pub keep_upper: bool,
    pub backend: TransactionalWorkspaceBackend,
}

enum MountBackendState {
    Kernel,
    #[cfg(target_os = "linux")]
    Fuse {
        child: std::process::Child,
    },
}

pub struct TransactionalWorkspaceMount {
    overlay_root: PathBuf,
    upper_dir: PathBuf,
    merged_dir: PathBuf,
    mounted: bool,
    keep_upper: bool,
    backend: MountBackendState,
}

impl TransactionalWorkspaceMount {
    #[must_use]
    pub fn merged_dir(&self) -> &Path {
        &self.merged_dir
    }

    #[must_use]
    pub fn uses_fuse_backend(&self) -> bool {
        matches!(self.backend, MountBackendState::Fuse { .. })
    }

    #[cfg(target_os = "linux")]
    pub fn cleanup(mut self) -> Result<()> {
        cleanup_linux(
            &self.overlay_root,
            &self.upper_dir,
            &self.merged_dir,
            self.keep_upper,
            self.mounted,
            &mut self.backend,
        )?;
        self.mounted = false;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub fn cleanup(self) -> Result<()> {
        let _ = self;
        Ok(())
    }
}

impl Drop for TransactionalWorkspaceMount {
    fn drop(&mut self) {
        #[cfg(target_os = "linux")]
        {
            if let Err(error) = cleanup_linux(
                &self.overlay_root,
                &self.upper_dir,
                &self.merged_dir,
                self.keep_upper,
                self.mounted,
                &mut self.backend,
            ) {
                tracing::warn!(error = %error, "Failed to clean up transactional workspace");
            } else {
                self.mounted = false;
            }
        }
    }
}

pub fn setup(
    workdir: Option<&str>,
    config: Option<&TransactionalWorkspaceConfig>,
    policy: &SandboxPolicy,
) -> Result<Option<TransactionalWorkspaceMount>> {
    let Some(config) = config else {
        return Ok(None);
    };
    if config.backend == TransactionalWorkspaceBackend::Off {
        return Ok(None);
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = workdir;
        let _ = config;
        let _ = policy;
        return Err(miette::miette!(
            "Transactional workspaces are only supported on Linux"
        ));
    }

    #[cfg(target_os = "linux")]
    {
        let lowerdir = workdir.ok_or_else(|| {
            miette::miette!("Transactional workspace requires an explicit workdir")
        })?;
        match setup_linux(lowerdir, config, policy) {
            Err(error) if error.to_string() == "AUTO_BACKEND_UNAVAILABLE" => Ok(None),
            other => other,
        }
    }
}

#[cfg(target_os = "linux")]
fn setup_linux(
    lowerdir: &str,
    config: &TransactionalWorkspaceConfig,
    policy: &SandboxPolicy,
) -> Result<Option<TransactionalWorkspaceMount>> {
    let lowerdir = fs::canonicalize(lowerdir)
        .into_diagnostic()
        .wrap_err("Failed to resolve transactional workspace lowerdir")?;
    let overlay_root = config.root.clone();
    let upper_dir = overlay_root.join("upper");
    let work_dir = overlay_root.join("work");
    let merged_dir = overlay_root.join("merged");

    fs::create_dir_all(&upper_dir)
        .into_diagnostic()
        .wrap_err("Failed to create transactional workspace upper dir")?;
    fs::create_dir_all(&work_dir)
        .into_diagnostic()
        .wrap_err("Failed to create transactional workspace work dir")?;
    fs::create_dir_all(&merged_dir)
        .into_diagnostic()
        .wrap_err("Failed to create transactional workspace merged dir")?;

    let lowerdir_str = lowerdir
        .to_str()
        .ok_or_else(|| miette::miette!("Transactional workspace path must be valid UTF-8"))?;
    let upperdir = upper_dir
        .to_str()
        .ok_or_else(|| miette::miette!("Transactional workspace path must be valid UTF-8"))?;
    let workdir = work_dir
        .to_str()
        .ok_or_else(|| miette::miette!("Transactional workspace path must be valid UTF-8"))?;
    let mount_opts = format!("lowerdir={lowerdir_str},upperdir={upperdir},workdir={workdir}");

    let backend = mount_transactional_workspace(config.backend, &merged_dir, &mount_opts)
        .wrap_err("Failed to activate transactional workspace backend")?;
    if matches!(backend, MountBackendState::Kernel) {
        align_mount_ownership(&lowerdir, &upper_dir, &work_dir, &merged_dir, policy)
            .wrap_err("Failed to align transactional workspace ownership")?;
    }

    Ok(Some(TransactionalWorkspaceMount {
        overlay_root,
        upper_dir,
        merged_dir,
        mounted: true,
        keep_upper: config.keep_upper,
        backend,
    }))
}

#[cfg(target_os = "linux")]
fn mount_transactional_workspace(
    backend: TransactionalWorkspaceBackend,
    target: &Path,
    mount_opts: &str,
) -> Result<MountBackendState> {
    match backend {
        TransactionalWorkspaceBackend::Off => {
            unreachable!("off backend should be filtered earlier")
        }
        TransactionalWorkspaceBackend::KernelOverlay => {
            mount_overlay(target, mount_opts)?;
            Ok(MountBackendState::Kernel)
        }
        TransactionalWorkspaceBackend::FuseOverlay => mount_fuse_overlay(target, mount_opts),
        TransactionalWorkspaceBackend::Auto => match mount_overlay(target, mount_opts) {
            Ok(()) => Ok(MountBackendState::Kernel),
            Err(error)
                if matches!(
                    root_cause_os_error_kind(&error),
                    Some(ErrorKind::PermissionDenied)
                ) =>
            {
                tracing::info!(
                    error = %error,
                    "Kernel OverlayFS unavailable, falling back to fuse-overlayfs"
                );
                match mount_fuse_overlay(target, mount_opts) {
                    Ok(state) => Ok(state),
                    Err(fuse_error) => {
                        tracing::warn!(
                            kernel_error = %error,
                            fuse_error = %fuse_error,
                            "Transactional workspace unavailable in auto mode; falling back to normal workdir"
                        );
                        Err(miette::miette!("AUTO_BACKEND_UNAVAILABLE"))
                    }
                }
            }
            Err(error) => Err(error),
        },
    }
}

#[cfg(target_os = "linux")]
fn root_cause_os_error_kind(error: &miette::Report) -> Option<ErrorKind> {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<std::io::Error>())
        .map(std::io::Error::kind)
}

#[cfg(target_os = "linux")]
fn mount_fuse_overlay(target: &Path, mount_opts: &str) -> Result<MountBackendState> {
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    let mut child = Command::new("fuse-overlayfs")
        .arg("-f")
        .arg("-o")
        .arg(mount_opts)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .into_diagnostic()
        .wrap_err("Failed to start fuse-overlayfs")?;

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if is_mountpoint(target)? {
            return Ok(MountBackendState::Fuse { child });
        }

        if let Some(status) = child.try_wait().into_diagnostic()? {
            return Err(miette::miette!(
                "fuse-overlayfs exited before mount became ready with status {status}"
            ));
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(miette::miette!(
                "Timed out waiting for fuse-overlayfs mount to become ready"
            ));
        }

        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(target_os = "linux")]
fn is_mountpoint(path: &Path) -> Result<bool> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo")
        .into_diagnostic()
        .wrap_err("Failed to read /proc/self/mountinfo")?;
    let target = path
        .to_str()
        .ok_or_else(|| miette::miette!("Transactional workspace path must be valid UTF-8"))?;
    Ok(mountinfo.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let _mount_id = fields.next();
        let _parent_id = fields.next();
        let _major_minor = fields.next();
        let _root = fields.next();
        matches!(fields.next(), Some(mount_point) if mount_point == target)
    }))
}

#[cfg(target_os = "linux")]
fn align_mount_ownership(
    lowerdir: &Path,
    upper_dir: &Path,
    work_dir: &Path,
    merged_dir: &Path,
    policy: &SandboxPolicy,
) -> Result<()> {
    let Some(user) = policy.process.run_as_user.as_deref() else {
        return Ok(());
    };

    let target_user = nix::unistd::User::from_name(user)
        .into_diagnostic()
        .wrap_err("Failed to resolve transactional workspace run user")?
        .ok_or_else(|| miette::miette!("Transactional workspace user '{user}' not found"))?;

    let target_gid = if let Some(group) = policy.process.run_as_group.as_deref() {
        nix::unistd::Group::from_name(group)
            .into_diagnostic()
            .wrap_err("Failed to resolve transactional workspace run group")?
            .ok_or_else(|| miette::miette!("Transactional workspace group '{group}' not found"))?
            .gid
    } else {
        target_user.gid
    };

    let dir_mode = fs::metadata(lowerdir)
        .into_diagnostic()
        .wrap_err("Failed to stat transactional workspace lowerdir")?
        .mode()
        & 0o7777;

    set_dir_owner_mode(
        upper_dir,
        target_user.uid.as_raw(),
        target_gid.as_raw(),
        dir_mode,
    )?;
    set_dir_owner_mode(
        work_dir,
        target_user.uid.as_raw(),
        target_gid.as_raw(),
        dir_mode,
    )?;
    set_dir_owner_mode(
        merged_dir,
        target_user.uid.as_raw(),
        target_gid.as_raw(),
        dir_mode,
    )?;

    Ok(())
}

#[cfg(target_os = "linux")]
fn set_dir_owner_mode(path: &Path, uid: u32, gid: u32, mode: u32) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).into_diagnostic()?;

    let current_uid = nix::unistd::geteuid().as_raw();
    let current_gid = nix::unistd::getegid().as_raw();
    let should_chown = current_uid == 0 || current_uid != uid || current_gid != gid;

    if should_chown {
        #[allow(unsafe_code)]
        let chown_rc = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
        if chown_rc != 0 {
            return Err(std::io::Error::last_os_error())
                .into_diagnostic()
                .wrap_err(format!(
                    "Failed to chown transactional workspace path {}",
                    path.display()
                ));
        }
    }

    #[allow(unsafe_code)]
    let chmod_rc = unsafe { libc::chmod(c_path.as_ptr(), mode) };
    if chmod_rc != 0 {
        return Err(std::io::Error::last_os_error())
            .into_diagnostic()
            .wrap_err(format!(
                "Failed to chmod transactional workspace path {}",
                path.display()
            ));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn cleanup_linux(
    overlay_root: &Path,
    upper_dir: &Path,
    merged_dir: &Path,
    keep_upper: bool,
    mounted: bool,
    backend: &mut MountBackendState,
) -> Result<()> {
    if mounted {
        unmount_workspace(merged_dir, backend)?;
    }

    if !keep_upper && upper_dir.exists() {
        fs::remove_dir_all(upper_dir)
            .into_diagnostic()
            .wrap_err("Failed to remove transactional workspace upper dir")?;
    }

    let work_dir = overlay_root.join("work");
    if work_dir.exists() {
        fs::remove_dir_all(&work_dir)
            .into_diagnostic()
            .wrap_err("Failed to remove transactional workspace work dir")?;
    }

    if merged_dir.exists() {
        fs::remove_dir_all(merged_dir)
            .into_diagnostic()
            .wrap_err("Failed to remove transactional workspace merged dir")?;
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn unmount_workspace(target: &Path, backend: &mut MountBackendState) -> Result<()> {
    match backend {
        MountBackendState::Kernel => unmount_overlay(target),
        MountBackendState::Fuse { child } => {
            unmount_fuse_overlay(target)?;
            let _ = child.wait();
            Ok(())
        }
    }
}

#[cfg(target_os = "linux")]
fn mount_overlay(target: &Path, mount_opts: &str) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new("overlay").into_diagnostic()?;
    let fstype = CString::new("overlay").into_diagnostic()?;
    let target = CString::new(target.as_os_str().as_bytes()).into_diagnostic()?;
    let data = CString::new(mount_opts).into_diagnostic()?;

    #[allow(unsafe_code)]
    let rc = unsafe {
        libc::mount(
            source.as_ptr(),
            target.as_ptr(),
            fstype.as_ptr(),
            0,
            data.as_ptr().cast(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .into_diagnostic()
            .wrap_err("Failed to mount OverlayFS transactional workspace");
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn unmount_overlay(target: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let target = CString::new(target.as_os_str().as_bytes()).into_diagnostic()?;
    #[allow(unsafe_code)]
    let rc = unsafe { libc::umount2(target.as_ptr(), libc::MNT_DETACH) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .into_diagnostic()
            .wrap_err("Failed to unmount OverlayFS transactional workspace");
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn unmount_fuse_overlay(target: &Path) -> Result<()> {
    use std::process::Command;

    let fusermount3 = Command::new("fusermount3").arg("-u").arg(target).status();
    match fusermount3 {
        Ok(status) if status.success() => return Ok(()),
        Ok(_) | Err(_) => {}
    }

    unmount_overlay(target).wrap_err("Failed to unmount fuse-overlayfs workspace")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backend_names() {
        assert_eq!(
            TransactionalWorkspaceBackend::parse("auto").expect("parse"),
            TransactionalWorkspaceBackend::Auto
        );
        assert_eq!(
            TransactionalWorkspaceBackend::parse("off").expect("parse"),
            TransactionalWorkspaceBackend::Off
        );
        assert_eq!(
            TransactionalWorkspaceBackend::parse("kernel").expect("parse"),
            TransactionalWorkspaceBackend::KernelOverlay
        );
        assert_eq!(
            TransactionalWorkspaceBackend::parse("fuse").expect("parse"),
            TransactionalWorkspaceBackend::FuseOverlay
        );
    }

    #[test]
    fn returns_none_when_disabled() {
        let mount = setup(
            Some("/tmp"),
            None,
            &SandboxPolicy {
                version: 1,
                filesystem: Default::default(),
                network: Default::default(),
                landlock: Default::default(),
                process: Default::default(),
            },
        )
        .expect("setup");
        assert!(mount.is_none());
    }
}
