use std::{ffi::OsString, os::windows::ffi::OsStringExt};

use itertools::Itertools;
use serde::Serialize;

use crate::{
    buckets::{Bucket, BucketError},
    Scoop,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Internal Windows API Error: {0}")]
    Windows(#[from] windows::core::Error),
    #[error("Interacting with buckets: {0}")]
    Bucket(#[from] BucketError),
    #[error("Error checking root privelages: {0}")]
    Quork(#[from] quork::root::Error),
}

#[derive(Debug, Copy, Clone, Serialize)]
pub enum LongPathsStatus {
    /// Long paths are enabled
    Enabled,
    /// This version of windows does not support long paths
    OldWindows,
    /// Long paths are disabled
    Disabled,
}

#[derive(Debug, Copy, Clone, Serialize)]
pub struct Helper {
    pub exe: &'static str,
    pub name: &'static str,
    pub reason: &'static str,
    pub packages: &'static [&'static str],
}

const EXPECTED_HELPERS: &[Helper] = &[
    Helper {
        exe: "7z",
        name: "7-Zip",
        reason: "unpacking most programs",
        packages: &["7zip", "7zip-std"],
    },
    Helper {
        exe: "innounp",
        name: "Inno Setup Unpacker",
        reason: "unpacking InnoSetup files",
        packages: &["innounp"],
    },
    Helper {
        exe: "dark",
        name: "Dark",
        reason: "unpacking installers created with the WiX toolkit",
        packages: &["dark", "wixtoolset"],
    },
];

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostics {
    pub git_installed: bool,
    pub long_paths: LongPathsStatus,
    pub main_bucket: bool,
    pub windows_developer: bool,
    pub windows_defender: bool,
    pub missing_helpers: Vec<Helper>,
    pub scoop_ntfs: bool,
}

impl Diagnostics {
    /// Collect all diagnostics
    ///
    /// # Errors
    /// - Unable to check long paths
    /// - Unable to check main bucket
    /// - Unable to check windows developer status
    /// - Unable to check windows defender status
    pub fn collect() -> Result<Self, Error> {
        let git_installed = Self::git_installed();
        debug!("Check git is installed");
        let main_bucket = Self::check_main_bucket()?;
        debug!("Checked main bucket");
        let long_paths = Self::check_long_paths()?;
        debug!("Checked long paths");
        let windows_developer = Self::get_windows_developer_status()?;
        debug!("Checked developer mode");

        let windows_defender = if crate::is_elevated()? {
            Self::check_windows_defender()?
        } else {
            false
        };
        debug!("Checked windows defender");

        let missing_helpers = EXPECTED_HELPERS
            .iter()
            .filter(|helper| which::which(helper.exe).is_err())
            .copied()
            .collect();

        let scoop_ntfs = Self::is_ntfs()?;

        Ok(Self {
            git_installed,
            long_paths,
            main_bucket,
            windows_developer,
            windows_defender,
            missing_helpers,
            scoop_ntfs,
        })
    }

    #[allow(unreachable_code)]
    /// Check if Windows Defender is ignoring the Scoop directory
    ///
    /// # Errors
    /// - Unable to read the registry
    /// - Unable to open the registry key
    /// - Unable to check if the key exists
    pub fn check_windows_defender() -> windows::core::Result<bool> {
        use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

        let scoop_dir = Scoop::path();
        let key = RegKey::predef(HKEY_LOCAL_MACHINE)
            .open_subkey(r"SOFTWARE\Microsoft\Windows Defender\Exclusions\Paths")?;

        Ok(key.open_subkey(scoop_dir).is_ok())
    }

    /// Check if the main bucket exists
    ///
    /// # Errors
    /// - Unable to list buckets
    pub fn check_main_bucket() -> Result<bool, BucketError> {
        let buckets = Bucket::list_all()?;

        Ok(buckets.into_iter().any(|bucket| bucket.name() == "main"))
    }

    /// Check if long paths are enabled
    ///
    /// # Errors
    /// - Unable to read the registry
    /// - Unable to read the OS version
    pub fn check_long_paths() -> windows::core::Result<LongPathsStatus> {
        use windows::Win32::System::SystemInformation::{
            GetVersionExW, OSVERSIONINFOEXW, OSVERSIONINFOW,
        };
        use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

        let version_info = unsafe {
            let mut version_info = OSVERSIONINFOEXW {
                #[allow(clippy::cast_possible_truncation)]
                dwOSVersionInfoSize: std::mem::size_of::<OSVERSIONINFOEXW>() as u32,
                ..std::mem::zeroed()
            };

            GetVersionExW(std::ptr::addr_of_mut!(version_info).cast::<OSVERSIONINFOW>())?;

            version_info
        };

        let major_version = version_info.dwMajorVersion;
        debug!("Windows Major Version: {major_version}");

        if major_version < 10 {
            return Ok(LongPathsStatus::OldWindows);
        }

        let hlkm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let key = hlkm.open_subkey(r"SYSTEM\CurrentControlSet\Control\FileSystem")?;

        if key.get_value::<u32, _>("LongPathsEnabled")? == 0 {
            Ok(LongPathsStatus::Disabled)
        } else {
            Ok(LongPathsStatus::Enabled)
        }
    }

    /// Check if the user has developer mode enabled
    ///
    /// # Errors
    /// - Unable to read the registry
    /// - Unable to read the value
    pub fn get_windows_developer_status() -> windows::core::Result<bool> {
        use winreg::{enums::HKEY_LOCAL_MACHINE, RegKey};

        let hlkm = RegKey::predef(HKEY_LOCAL_MACHINE);
        let key = hlkm.open_subkey(r"SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock")?;

        Ok(key.get_value::<u32, _>("AllowDevelopmentWithoutDevLicense")? == 1)
    }

    /// Check if the Scoop directory is on an NTFS filesystem
    ///
    /// # Errors
    /// - Unable to get the volume information
    /// - Unable to check the filesystem
    /// - Unable to get the root path
    pub fn is_ntfs() -> windows::core::Result<bool> {
        use windows::{
            core::HSTRING,
            Win32::{Foundation::MAX_PATH, Storage::FileSystem::GetVolumeInformationW},
        };

        let path = Scoop::path();

        let root = {
            let mut current = path.as_path();

            while let Some(parent) = current.parent() {
                current = parent;
            }

            debug!("Checking filesystem of: {}", current.display());

            current
        };

        let mut fs_name = [0u16; MAX_PATH as usize];

        unsafe {
            GetVolumeInformationW(
                &HSTRING::from(root),
                None,
                None,
                // &mut max_component_length,
                None,
                // &mut flags,
                None,
                Some(&mut fs_name),
            )?;
        }

        debug!("Filesystem: {:?}", OsString::from_wide(&fs_name));

        Ok(fs_name.starts_with(&"NTFS".encode_utf16().collect_vec()))
    }

    #[must_use]
    /// Check if the user has git installed
    pub fn git_installed() -> bool {
        which::which("git").is_ok()
    }
}