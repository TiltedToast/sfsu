use std::{fs::File, io::Read, path::Path};

use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use regex::Regex;
use serde::Deserialize;

use crate::{
    buckets::{self, Bucket},
    get_scoop_path,
};

pub mod install;
pub mod manifest;

pub use install::Manifest as InstallManifest;
pub use manifest::Manifest;

use manifest::StringOrArrayOfStringsOrAnArrayOfArrayOfStrings;

#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    #[error("Invalid utf8 found. This is not supported by sfsu")]
    NonUtf8,
    #[error("Missing file name. The path terminated in '..'")]
    MissingFileName,
    #[error("{0}")]
    IO(#[from] std::io::Error),
    #[error("Could not parse manifest \"{0}\". Failed with error: {1}")]
    ParsingManifest(String, serde_json::Error),
    #[error("Interacting with buckets: {0}")]
    BucketError(#[from] buckets::BucketError),
}

#[derive(Debug, Clone)]
pub enum PackageReference {
    BucketNamePair { bucket: String, name: String },
    Name(String),
}

pub type Result<T> = std::result::Result<T, PackageError>;

pub trait CreateManifest
where
    Self: Default + for<'a> Deserialize<'a>,
{
    /// Convert a path into a manifest
    ///
    /// # Errors
    /// - The file does not exist
    /// - The file was not valid UTF-8
    fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let contents = std::fs::read_to_string(path)?;

        Self::from_str(contents)
            // TODO: Maybe figure out a better approach to this, but it works for now
            .map(|s| s.with_name(path))
            .map_err(|e| PackageError::ParsingManifest(path.display().to_string(), e))
    }

    /// # Errors
    /// - The contents are not a valid manifest
    fn from_str(contents: String) -> serde_json::Result<Self> {
        let trimmed = contents.trim_start_matches('\u{feff}');

        serde_json::from_str(trimmed)
    }

    #[must_use]
    fn with_name(self, path: &Path) -> Self;
}

impl CreateManifest for Manifest {
    fn with_name(mut self, path: &Path) -> Self {
        self.name = path
            .with_extension("")
            .file_name()
            .map(|f| f.to_string_lossy())
            .expect("File to have file name")
            .to_string();

        self
    }
}

impl CreateManifest for InstallManifest {
    fn with_name(mut self, path: &Path) -> Self {
        self.name = path
            .with_extension("")
            .file_name()
            .map(|f| f.to_string_lossy())
            .expect("File to have name")
            .to_string();

        self
    }
}

impl InstallManifest {
    /// List all install manifests
    ///
    /// # Errors
    /// - Invalid install manifest
    /// - Reading directories fails
    pub fn list_all() -> Result<Vec<Self>> {
        crate::list_scoop_apps()?
            .par_iter()
            .map(|path| Self::from_path(path.join("current/install.json")))
            .collect::<Result<Vec<_>>>()
    }
}

impl Manifest {
    /// Gets the manifest from a bucket and manifest name
    ///
    /// # Errors
    /// - If the manifest doesn't exist or bucket is invalid
    pub fn from_reference((bucket, name): (String, String)) -> Result<Self> {
        Bucket::new(bucket)?.get_manifest(name)
    }

    #[must_use]
    pub fn binary_matches(&self, regex: &Regex) -> Option<Vec<String>> {
        match self.bin {
            Some(StringOrArrayOfStringsOrAnArrayOfArrayOfStrings::String(ref binary)) => {
                if regex.is_match(binary) {
                    Some(vec![binary.clone()])
                } else {
                    None
                }
            }
            Some(StringOrArrayOfStringsOrAnArrayOfArrayOfStrings::StringArray(ref binaries)) => {
                let matched: Vec<_> = binaries
                    .iter()
                    .filter(|binary| regex.is_match(binary))
                    .cloned()
                    .collect();

                if matched.is_empty() {
                    None
                } else {
                    Some(matched)
                }
            }
            _ => None,
        }
    }

    /// List all installed app manifests
    ///
    /// # Errors
    /// - Invalid install manifest
    /// - Reading directories fails
    ///
    /// # Panics
    /// - If the file name is invalid
    pub fn list_installed() -> Result<Vec<Result<Self>>> {
        Ok(crate::list_scoop_apps()?
            .par_iter()
            .map(|path| {
                Self::from_path(path.join("current/manifest.json")).and_then(|mut manifest| {
                    manifest.name = path
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .ok_or(PackageError::MissingFileName)?;

                    Ok(manifest)
                })
            })
            .collect::<Vec<_>>())
    }
}

/// Check if the manifest path is installed, and optionally confirm the bucket
///
/// # Panics
/// - The file was not valid UTF-8
pub fn is_installed(manifest_name: impl AsRef<Path>, bucket: Option<impl AsRef<str>>) -> bool {
    let scoop_path = get_scoop_path();
    let installed_path = scoop_path
        .join("apps")
        .join(manifest_name)
        .join("current/install.json");

    match InstallManifest::from_path(installed_path) {
        Ok(manifest) => {
            if let Some(bucket) = bucket {
                manifest.get_source() == bucket.as_ref()
            } else {
                false
            }
        }
        Err(_) => false,
    }
}
