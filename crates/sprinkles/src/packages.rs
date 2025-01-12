//! Scoop package helpers

use std::{
    path::Path,
    process::Stdio,
    time::{SystemTimeError, UNIX_EPOCH},
};

use chrono::{DateTime, Local};
use git2::Commit;
use gix::{object::tree::diff::Action, traverse::commit::simple::Sorting};
use itertools::Itertools;
use quork::traits::truthy::ContainsTruth as _;
use rayon::iter::{IntoParallelRefIterator, ParallelIterator};
use regex::Regex;
use serde::{Deserialize, Serialize};
use strum::Display;

use crate::{
    buckets::{self, Bucket},
    config,
    contexts::ScoopContext,
    git::{self, errors, Repo},
    let_chain,
    output::{
        sectioned::{Children, Section, Text},
        wrappers::{author::Author, time::NicerTime},
    },
    Architecture,
};

#[cfg(feature = "manifest-hashes")]
use crate::{
    hash::{
        self,
        substitutions::{Substitute, SubstitutionMap},
    },
    packages::manifest::TOrArrayOfTs,
};

pub mod downloading;
pub mod models;
pub mod reference;

pub use models::{install::Manifest as InstallManifest, manifest::Manifest};

use downloading::DownloadUrl;
use models::manifest::{InstallConfig, StringArray};

#[macro_export]
/// Get a field from a manifest based on the architecture
macro_rules! arch_config {
    ($field:ident.$arch:expr) => {
        match $arch {
            $crate::Architecture::Arm64 => $field.arm64.as_ref(),
            $crate::Architecture::X64 => $field.x64.as_ref(),
            $crate::Architecture::X86 => $field.x86.as_ref(),
        }
    };

    ($field:ident) => {
        arch_config!($field.$crate::Architecture::ARCH)
    };

    ($field:ident.$arch:expr => clone) => {
        arch_config!($field.$arch).cloned()
    };

    ($field:ident => clone) => {
        arch_config!($field).cloned()
    };

    // ($field:ident.$arch:expr => $default:expr) => {
    //     arch_config!($field.$arch).unwrap_or($default)
    // };

    // ($field:ident => $default:expr) => {
    //     arch_config!($field.$crate::Architecture::ARCH).unwrap_or($default)
    // };
}

#[macro_export]
/// Get a field from a manifest based on the architecture
macro_rules! arch_field {
    // ($self:ident.$field:ident) => {
    //     arch_field!($self.$field).clone()
    // };

    // ($arch:expr => ref $self:ident.$field:ident) => {{
    //     if let Some(cfg) = match $arch {
    //         $crate::Architecture::Arm64 => &$self.arm64,
    //         $crate::Architecture::X64 => &$self.x64,
    //         $crate::Architecture::X86 => &$self.x86,
    //     } {
    //         &cfg.$field
    //     } else {
    //         &None
    //     }
    // }};

    // (ref $self:ident.$field:ident) => {
    //     arch_field!($crate::Architecture::ARCH => ref $self.$field)
    // };

    // ($arch:expr => ref mut $self:ident.$field:ident) => {{
    //     match $arch {
    //         $crate::Architecture::Arm64 => $self.arm64.as_mut(),
    //         $crate::Architecture::X64 => $self.x64.as_mut(),
    //         $crate::Architecture::X86 => $self.x86.as_mut(),
    //     }.and_then(|cfg| cfg.$field.as_mut())
    // }};

    // (ref mut $self:ident.$field:ident) => {
    //     arch_field!($crate::Architecture::ARCH => ref mut $self.$field)
    // };

    ($self:ident.$field:ident as cloned) => {
        arch_field!($crate::Architecture::ARCH => $self.$field as ref).cloned()
    };

    ($arch:expr => $self:ident.$field:ident as cloned) => {
        arch_field!($arch => $self.$field as ref).cloned()
    };

    ($self:ident.$field:ident as ref) => {
        arch_field!($crate::Architecture::ARCH => $self.$field as ref)
    };

    ($arch:expr => $self:ident.$field:ident as ref) => {{
        match $arch {
            $crate::Architecture::Arm64 => $self.arm64.as_ref(),
            $crate::Architecture::X64 => $self.x64.as_ref(),
            $crate::Architecture::X86 => $self.x86.as_ref(),
        }.and_then(|cfg| cfg.$field.as_ref())
    }};

    ($self:ident.$field:ident as mut) => {
        arch_field!($crate::Architecture::ARCH => $self.$field as mut)
    };

    ($arch:expr => $self:ident.$field:ident as mut) => {{
        match $arch {
            $crate::Architecture::Arm64 => $self.arm64.as_mut(),
            $crate::Architecture::X64 => $self.x64.as_mut(),
            $crate::Architecture::X86 => $self.x86.as_mut(),
        }.and_then(|cfg| cfg.$field.as_mut())
    }};
}

pub use arch_config;
pub use arch_field;

use self::models::manifest::{
    self, AliasArray, AutoupdateArchitecture, AutoupdateConfig, HashExtraction,
    HashExtractionOrArrayOfHashExtractions, ManifestArchitecture,
};

#[derive(Debug, Serialize)]
/// Minimal package info
pub struct MinInfo {
    /// The name of the package
    pub name: String,
    /// The version of the package
    pub version: String,
    /// The package's source (eg. bucket name)
    pub source: String,
    /// The last time the package was updated
    pub updated: NicerTime<DateTime<Local>>,
    /// The package's notes
    pub notes: String,
}

impl MinInfo {
    /// Parse minmal package info for every installed app
    ///
    /// # Errors
    /// - Invalid file names
    /// - File metadata errors
    /// - Invalid time
    pub fn list_installed(
        ctx: &impl ScoopContext<config::Scoop>,
        bucket: Option<&String>,
    ) -> Result<Vec<Self>> {
        let apps = ctx.installed_apps()?;

        apps.par_iter()
            .map(Self::from_path)
            .filter(|package| {
                if let Ok(pkg) = package {
                    if let Some(bucket) = bucket {
                        return &pkg.source == bucket;
                    }
                }
                // Keep errors so that the following line will return the error
                true
            })
            .collect()
    }

    /// Parse minimal package into from a given path
    ///
    /// # Errors
    /// - Invalid file names
    /// - File metadata errors
    /// - Invalid time
    ///
    /// # Panics
    /// - Date time invalid or out of range
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        let package_name = path
            .file_name()
            .map(|f| f.to_string_lossy())
            .ok_or(Error::MissingFileName)?;

        let updated_time = {
            let updated = {
                let updated_sys = path.metadata()?.modified()?;

                updated_sys.duration_since(UNIX_EPOCH)?.as_secs()
            };

            #[allow(clippy::cast_possible_wrap)]
            DateTime::from_timestamp(updated as i64, 0)
                .expect("invalid or out-of-range datetime")
                .with_timezone(&Local)
        };

        let app_current = path.join("current");

        let manifest = Manifest::from_path(app_current.join("manifest.json")).unwrap_or_default();

        let install_manifest =
            InstallManifest::from_path(app_current.join("install.json")).unwrap_or_default();

        Ok(Self {
            name: package_name.to_string(),
            version: manifest.version.to_string(),
            source: install_manifest.get_source(),
            updated: updated_time.into(),
            notes: if install_manifest.hold.contains_truth() {
                String::from("Held")
            } else {
                String::new()
            },
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
/// Package error
pub enum Error {
    #[error("Invalid utf8 found. This is not supported by sfsu")]
    NonUtf8,
    #[error("Missing or invalid file name. The path terminated in '..' or wasn't valid utf8")]
    MissingFileName,
    #[error("{0}")]
    IO(#[from] std::io::Error),
    #[error("Could not parse manifest \"{0}\". Failed with error: {1}")]
    ParsingManifest(String, serde_json::Error),
    #[error("Interacting with buckets: {0}")]
    BucketError(#[from] buckets::Error),
    #[error("Interacting with git2: {0}")]
    RepoError(#[from] git::Error),
    #[error("git2 internal error: {0}")]
    Git2Error(#[from] git2::Error),
    #[error("System Time: {0}")]
    TimeError(#[from] SystemTimeError),
    #[error("Could not find executable in path: {0}")]
    MissingInPath(#[from] which::Error),
    #[error("Gitoxide error: {0}")]
    Gitoxide(#[from] Box<errors::GitoxideError>),
    #[error("Git delta did not have a path")]
    DeltaNoPath,
    #[error("Cannot find git commit where package was updated")]
    NoUpdatedCommit,
    #[error("Invalid time. (time went backwards or way way way too far forwards (hello future! whats it like?))")]
    InvalidTime,
    #[error("Invalid timezone provided. (where are you?)")]
    InvalidTimeZone,
    #[error("Git provided no output")]
    MissingGitOutput,
    #[error("Missing local manifest for package")]
    MissingLocalManifest,
    #[cfg(feature = "manifest-hashes")]
    #[error("Could not get hash for app: {0}")]
    HashError(#[from] hash::Error),
    #[error("Manifest does not have `autoupdate` field")]
    MissingAutoUpdate,
    #[error("Manifest architecture section does not have `autoupdate` field")]
    MissingArchAutoUpdate,
    #[error("Commit did not have a parent")]
    MissingParent,
}

/// The result type for package operations
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Default, Copy, Clone, Display, PartialEq, Eq)]
#[strum(serialize_all = "snake_case")]
/// The search mode
pub enum SearchMode {
    #[default]
    /// Only search the name
    Name,
    /// Only search the binaries
    Binary,
    /// Search both the name and binaries
    Both,
}

#[cfg(feature = "clap")]
impl clap::ValueEnum for SearchMode {
    fn value_variants<'a>() -> &'a [Self] {
        &[Self::Name, Self::Binary, Self::Both]
    }
    fn to_possible_value<'a>(&self) -> Option<clap::builder::PossibleValue> {
        match self {
            Self::Name => {
                Some(clap::builder::PossibleValue::new("name").help("Only search the name"))
            }
            Self::Binary => Some({
                clap::builder::PossibleValue::new("binary").help("Only search the binaries")
            }),
            Self::Both => Some({
                clap::builder::PossibleValue::new("both").help("Search both the name and binaries")
            }),
        }
    }
}

impl SearchMode {
    #[must_use]
    /// Check if the search mode matches names
    pub fn match_names(self) -> bool {
        matches!(self, SearchMode::Name | SearchMode::Both)
    }

    #[must_use]
    /// Check if the search mode only matches names
    pub fn only_match_names(self) -> bool {
        self == SearchMode::Name
    }

    #[must_use]
    /// Check if the search mode matches binaries
    pub fn match_binaries(self) -> bool {
        matches!(self, SearchMode::Binary | SearchMode::Both)
    }

    #[must_use]
    /// Check if the search mode only matches binaries
    pub fn only_match_binaries(self) -> bool {
        self == SearchMode::Binary
    }

    #[must_use]
    /// Check if the search mode matches both names and binaries
    ///
    /// Checks name first to avoid unnecessary binary checks
    pub fn eager_name_matches(self, manifest_name: &str, search_regex: &Regex) -> bool {
        if self.only_match_names() && search_regex.is_match(manifest_name) {
            return true;
        }
        if self.match_binaries() {
            return true;
        }

        false
    }
}

#[derive(Debug, Clone)]
#[must_use = "MatchCriteria has no side effects"]
/// The criteria for a match
pub struct MatchCriteria {
    name: bool,
    bins: Vec<String>,
}

impl MatchCriteria {
    /// Create a new match criteria
    pub const fn new() -> Self {
        Self {
            name: false,
            bins: vec![],
        }
    }

    /// Check if the name matches
    pub fn matches(
        file_name: &str,
        manifest: Option<&Manifest>,
        mode: SearchMode,
        pattern: &Regex,
        arch: Architecture,
    ) -> Self {
        let file_name = file_name.to_string();

        let mut output = MatchCriteria::new();

        if mode.match_names() && pattern.is_match(&file_name) {
            output.name = true;
        }

        if let Some(manifest) = manifest {
            let binaries = manifest
                .architecture
                .merge_default(manifest.install_config.clone(), arch)
                .bin
                .map(|b| b.to_vec())
                .unwrap_or_default();

            let binary_matches = binaries
                .into_iter()
                .filter(|binary| pattern.is_match(binary))
                .filter_map(|b| {
                    if pattern.is_match(&b) {
                        Some(b.clone())
                    } else {
                        None
                    }
                });

            output.bins.extend(binary_matches);
        }

        output
    }
}

impl Default for MatchCriteria {
    fn default() -> Self {
        Self::new()
    }
}

/// Localised functions for creating manifests
pub trait CreateManifest
where
    Self: for<'a> Deserialize<'a>,
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
            .map(|s| s.with_name(path).with_bucket(path))
            .map_err(|e| Error::ParsingManifest(path.display().to_string(), e))
    }

    /// # Errors
    /// - The contents are not a valid manifest
    fn from_str(contents: String) -> serde_json::Result<Self> {
        let trimmed = contents.trim_start_matches('\u{feff}');

        serde_json::from_str(trimmed)
    }

    /// Set the name of the manifest. Not meant to be used directly.
    #[must_use]
    fn with_name(self, path: impl AsRef<Path>) -> Self;

    /// Set the bucket of the manifest. Not meant to be used directly.
    #[must_use]
    fn with_bucket(self, path: impl AsRef<Path>) -> Self;
}

impl CreateManifest for Manifest {
    fn with_name(mut self, path: impl AsRef<Path>) -> Self {
        self.name = path
            .as_ref()
            .with_extension("")
            .file_name()
            .map(|f| f.to_string_lossy())
            .expect("File to have file name")
            .to_string();

        self
    }

    fn with_bucket(mut self, path: impl AsRef<Path>) -> Self {
        self.bucket = path
            .as_ref()
            .parent()
            .and_then(|p| p.parent())
            .and_then(|bucket| bucket.file_name().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or_default();

        self
    }
}

impl CreateManifest for InstallManifest {
    fn with_name(mut self, path: impl AsRef<Path>) -> Self {
        self.name = path
            .as_ref()
            .with_extension("")
            .file_name()
            .map(|f| f.to_string_lossy())
            .expect("File to have name")
            .to_string();

        self
    }

    fn with_bucket(self, _path: impl AsRef<Path>) -> Self {
        self
    }
}

impl InstallManifest {
    /// List all install manifests
    ///
    /// # Errors
    /// - Invalid install manifest
    /// - Reading directories fails
    pub fn list_all(ctx: &impl ScoopContext<config::Scoop>) -> Result<Vec<Self>> {
        ctx.installed_apps()?
            .par_iter()
            .map(|path| Self::from_path(path.join("current/install.json")))
            .collect::<Result<Vec<_>>>()
    }

    /// List all install manifests, ignoring errors
    ///
    /// # Errors
    /// - Reading directories fails
    pub fn list_all_unchecked(ctx: &impl ScoopContext<config::Scoop>) -> Result<Vec<Self>> {
        Ok(ctx
            .installed_apps()?
            .par_iter()
            .filter_map(
                |path| match Self::from_path(path.join("current/install.json")) {
                    Ok(v) => Some(v.with_name(path)),
                    Err(_) => None,
                },
            )
            .collect::<Vec<_>>())
    }
}

impl Manifest {
    #[must_use]
    /// Get the install config for a given architecture
    pub fn install_config(&self, arch: Architecture) -> InstallConfig {
        self.architecture
            .as_ref()
            .merge_default(self.install_config.clone(), arch)
    }

    #[must_use]
    /// Get the autoupdate config for the default architecture
    pub fn autoupdate_config(&self, arch: Architecture) -> Option<AutoupdateConfig> {
        let autoupdate = self.autoupdate.as_ref()?;

        Some(
            autoupdate
                .architecture
                .clone()
                .merge_default(autoupdate.default_config.clone(), arch),
        )
    }

    #[must_use]
    /// Get the download urls for a given architecture
    pub fn download_urls(&self, arch: Architecture) -> Option<Vec<DownloadUrl>> {
        let urls = self.install_config(arch).url?;

        Some(
            urls.to_vec()
                .into_iter()
                .map(DownloadUrl::from_string)
                .collect(),
        )
    }

    #[must_use]
    /// Apply a bucket to a manifest
    pub fn with_bucket(mut self, bucket: &Bucket) -> Self {
        self.bucket = bucket.name().to_string();

        self
    }

    #[must_use]
    /// List the dependencies of a given manifest, in the order that they will be installed
    ///
    /// Note that this does not include the package itself as a dependency
    pub fn depends(&self) -> Vec<reference::ManifestRef> {
        self.depends
            .clone()
            .map(manifest::TOrArrayOfTs::to_vec)
            .unwrap_or_default()
    }

    /// Gets the manifest from a bucket and manifest name
    ///
    /// # Errors
    /// - If the manifest doesn't exist or bucket is invalid
    pub fn from_reference(
        ctx: &impl ScoopContext<config::Scoop>,
        (bucket, name): (String, String),
    ) -> Result<Self> {
        Bucket::from_name(ctx, bucket)?.get_manifest(name)
    }

    #[must_use]
    /// Check if the manifest binaries matche the given regex
    pub fn binary_matches(&self, regex: &Regex, arch: Architecture) -> Option<Vec<String>> {
        match self
            .architecture
            .as_ref()
            .merge_default(self.install_config.clone(), arch)
            .bin
        {
            Some(AliasArray::NestedArray(StringArray::Single(ref binary))) => {
                if regex.is_match(binary) {
                    Some(vec![binary.to_string()])
                } else {
                    None
                }
            }
            Some(AliasArray::NestedArray(StringArray::Array(ref binaries))) => {
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
    pub fn list_installed(ctx: &impl ScoopContext<config::Scoop>) -> Result<Vec<Result<Self>>> {
        Ok(ctx
            .installed_apps()?
            .par_iter()
            .map(|path| {
                Self::from_path(path.join("current/manifest.json")).and_then(|mut manifest| {
                    manifest.name = path
                        .file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .ok_or(Error::MissingFileName)?;

                    Ok(manifest)
                })
            })
            .collect::<Vec<_>>())
    }

    #[doc(hidden)]
    pub fn parse_output(
        &self,
        ctx: &impl ScoopContext<config::Scoop>,
        bucket: impl AsRef<str>,
        installed_only: bool,
        pattern: &Regex,
        mode: SearchMode,
        arch: Architecture,
    ) -> Option<Section<Text<String>>> {
        // TODO: Better display of output

        // This may be a bit of a hack, but it works

        let match_output = MatchCriteria::matches(
            &self.name,
            if mode.match_binaries() {
                Some(self)
            } else {
                None
            },
            mode,
            pattern,
            arch,
        );

        if !match_output.name && match_output.bins.is_empty() {
            return None;
        }

        let is_installed = self.is_installed(ctx, Some(bucket.as_ref()));
        if installed_only && !is_installed {
            return None;
        }

        let styled_package_name = if self.name == pattern.to_string() {
            console::style(&self.name).bold().to_string()
        } else {
            self.name.clone()
        };

        let installed_text = if is_installed && !installed_only {
            "[installed] "
        } else {
            ""
        };

        let title = format!("{styled_package_name} ({}) {installed_text}", self.version);

        let package = if mode.match_binaries() {
            let bins = match_output
                .bins
                .iter()
                .map(|output| {
                    Text::new(format!(
                        "{}{}",
                        crate::output::WHITESPACE,
                        console::style(output).bold()
                    ))
                })
                .collect_vec();

            Section::new(Children::from(bins))
        } else {
            Section::new(Children::None)
        }
        .with_title(title);

        Some(package)
    }

    #[must_use]
    /// Check if the manifest is installed
    pub fn is_installed(
        &self,
        ctx: &impl ScoopContext<config::Scoop>,
        bucket: Option<&str>,
    ) -> bool {
        is_installed(ctx, &self.name, bucket)
    }

    fn update_field<T>(
        arch_field: Option<&mut T>,
        default_field: &mut Option<T>,
        value: Option<T>,
    ) {
        let_chain!(let Some(arch_field) = arch_field; let Some(value) = value; {
            *arch_field = value;
        }; else {
            *default_field = value;
        });
        // if let Some(arch_field) = arch_field
        //     && let Some(value) = value
        // {
        //     *arch_field = value;
        // } else {
        //     *default_field = value;
        // }
    }

    #[cfg(feature = "manifest-hashes")]
    fn get_new_urls(&self, autoupdate: &AutoupdateConfig) -> Option<TOrArrayOfTs<String>> {
        use crate::hash::substitutions::Substitute;

        if let Some(autoupdate_urls) = &autoupdate.url {
            debug!("Autoupdate Urls: {autoupdate_urls}");

            let mut submap = SubstitutionMap::new();
            submap.append_version(&self.version);

            let new_urls = autoupdate_urls
                .clone()
                .to_vec()
                .into_iter()
                .map(|url| url.into_substituted(&submap, false));

            Some(new_urls.collect())
        } else {
            None
        }
    }

    #[cfg(feature = "manifest-hashes")]
    /// Set the manifest version and get the hash for the manifest
    ///
    /// # Errors
    /// - Missing autoupdate field
    /// - Hash error
    pub async fn set_version(
        &mut self,
        ctx: &impl ScoopContext<config::Scoop>,
        version: String,
    ) -> Result<(), Error> {
        use quork::traits::list::ListVariants;

        use crate::hash::Hash;

        self.version = version.into();

        let autoupdate = self.autoupdate.as_ref().ok_or(Error::MissingAutoUpdate)?;

        // TODO: This sets the same hash and url for all architectures
        for arch in crate::Architecture::VARIANTS {
            let arch_autoupdate = autoupdate
                .architecture
                .merge_default(autoupdate.default_config.clone(), arch);

            let arch_url = self.get_new_urls(&arch_autoupdate);

            if let Some(arch_config) = &mut self.architecture {
                Self::update_field(
                    arch_field!(arch => arch_config.url as mut),
                    &mut self.install_config.url,
                    arch_url,
                );
            } else {
                self.install_config.url = self.get_new_urls(&autoupdate.default_config);
            }
        }

        macro_rules! update_field {
            ($field:ident) => {{
                let mut submap = SubstitutionMap::new();
                submap.append_version(&self.version);

                for arch in crate::Architecture::VARIANTS {
                    if let Some(config) = &mut self.architecture {
                        let default = arch_field!(arch => config.$field as cloned).map(|s| s.into_substituted(&submap, false));

                        Self::update_field(
                            arch_field!(arch => config.$field as mut),
                            &mut self.install_config.$field,
                            default,
                        );
                    } else {
                        self.install_config.$field = autoupdate.default_config.$field.clone().map(|s| s.into_substituted(&submap, false));
                    }
                }
            }}
        }

        update_field!(bin);
        update_field!(extract_dir);
        update_field!(installer);
        update_field!(shortcuts);

        for arch in crate::Architecture::VARIANTS {
            let Ok(hashes) = Hash::get_for_app(ctx, self, arch).await else {
                continue;
            };

            if let Some(arch_config) = &mut self.architecture {
                // TODO: This sets the same hash and url for all architectures
                Self::update_field(
                    arch_field!(arch => arch_config.hash as mut),
                    &mut self.install_config.hash,
                    TOrArrayOfTs::from_vec(hashes),
                );
            } else {
                self.install_config.hash = TOrArrayOfTs::from_vec(hashes);
            }
        }

        // TODO: Handle other autoupdate fields
        // TODO: Autoupdate fields in all architectures
        // todo!("Handle urls and other autoupdate fields");

        // TODO: Figure out hash extraction
        // autoupdate_arch.hash

        // todo!()

        let workspace_manifest_path = ctx.workspace_path().join(format!("{}.json", self.name));
        serde_json::to_writer_pretty(std::fs::File::create(workspace_manifest_path)?, &self)
            .map_err(|e| {
                error!("Failed to write workspace manifest: {e}");
                Error::ParsingManifest(self.name.to_string(), e)
            })?;

        Ok(())
    }

    #[must_use]
    /// Check if the commit's message matches the name of the manifest
    pub fn commit_message_matches(&self, commit: &Commit<'_>) -> bool {
        if let Some(message) = commit.message() {
            message.starts_with(&self.name)
        } else {
            false
        }
    }

    /// Check if the commit's changed files matches the name of the manifest
    ///
    /// # Errors
    /// - Git2 errors
    pub fn commit_diff_matches(&self, repo: &Repo, commit: &Commit<'_>) -> Result<bool> {
        let mut diff_options = Repo::default_diff_options();

        let tree = commit.tree()?;
        let parent_tree = commit.parent(0)?.tree()?;

        let manifest_path = format!("bucket/{}.json", self.name);

        let diff = repo.diff_tree_to_tree(
            Some(&parent_tree),
            Some(&tree),
            Some(diff_options.pathspec(&manifest_path)),
        )?;

        // Given that the diffoptions ensure that we only match the specific manifest
        // we are safe to return as soon as we find a commit thats changed anything
        Ok(diff.stats()?.files_changed() != 0)
    }

    /// Get the time and author of the commit where this manifest was last changed
    ///
    /// # Errors
    /// - Invalid bucket
    /// - Invalid repo bucket
    /// - Internal git2 errors
    pub fn last_updated_info(
        &self,
        ctx: &impl ScoopContext<config::Scoop>,
        hide_emails: bool,
        disable_git: bool,
    ) -> Result<(Option<String>, Option<String>)> {
        let bucket = Bucket::from_name(ctx, &self.bucket)?;

        if disable_git {
            let repo = Repo::from_bucket(&bucket)?.to_gitoxide()?;
            let latest_commit = repo.head_commit().map_err(git::Error::from)?;

            let revwalk = repo
                .rev_walk([latest_commit.id])
                .sorting(Sorting::ByCommitTimeNewestFirst);

            let updated_commit = revwalk
                .all()
                .map_err(git::Error::from)?
                // .skip(1)
                .find_map(|info| {
                    let find_commit = || {
                        // TODO: Add tests using personal bucket to ensure that different methods return the same info
                        let info = info.map_err(git::Error::from)?;
                        let commit = info.object().map_err(git::Error::from)?;

                        #[cfg(not(feature = "info-difftrees"))]
                        if self.commit_message_matches(&commit) {
                            return Ok(commit);
                        }

                        #[cfg(feature = "info-difftrees")]
                        {
                            let mut matches = false;

                            let other = info.parent_ids().next().ok_or(Error::MissingParent)?;
                            let other = other.object().map_err(git::Error::from)?;
                            let other_tree = other.peel_to_tree().map_err(git::Error::from)?;
                            commit
                                .tree()
                                .map_err(git::Error::from)?
                                .changes()
                                .map_err(git::Error::from)?
                                .track_filename()
                                .for_each_to_obtain_tree(&other_tree, |change| {
                                    debug!("{change:?}");
                                    debug!("Filename: {}", change.location.to_string());

                                    if change.location.to_string().starts_with(&self.name) {
                                        matches = true;
                                        Ok::<_, Error>(Action::Cancel)
                                    } else {
                                        Ok(Action::Continue)
                                    }
                                })
                                .map_err(git::Error::from)?;

                            if matches {
                                return Ok(commit);
                            }
                        }

                        Err(Error::NoUpdatedCommit)
                    };

                    let result = find_commit();

                    match result {
                        Ok(commit) => Some(Ok(commit)),
                        Err(Error::NoUpdatedCommit) => None,
                        Err(e) => Some(Err(e)),
                    }
                })
                .ok_or(Error::NoUpdatedCommit)??;

            let date_time = git::parity::Time::from(
                updated_commit
                    .time()
                    .map_err(git::errors::GitoxideError::from)
                    .map_err(Box::new)?,
            )
            .to_datetime()
            .ok_or(Error::InvalidTime)?;

            let author_wrapped = Author::from(updated_commit.author().map_err(git::Error::from)?)
                .with_show_emails(!hide_emails);

            Ok((
                Some(date_time.to_string()),
                Some(author_wrapped.to_string()),
            ))
        } else {
            let output = bucket
                .open_repo()?
                .log("bucket", 1, "%aD#%an")?
                .arg(self.name.clone() + ".json")
                .stderr(Stdio::null())
                .output()
                .map_err(|_| Error::MissingGitOutput)?;

            let info = String::from_utf8(output.stdout)
                .map_err(|_| Error::NonUtf8)?
                // Remove newline from end
                .trim_end()
                // Remove weird single quote from either end
                .trim_matches('\'')
                .split_once('#')
                .map(|(time, author)| (time.to_string(), author.to_string()))
                .unzip();

            Ok(info)
        }
    }

    /// Get [`InstallManifest`] for [`Manifest`]
    ///
    /// # Errors
    /// - Missing or invalid [`InstallManifest`]
    pub fn install_manifest(
        &self,
        ctx: &impl ScoopContext<config::Scoop>,
    ) -> Result<InstallManifest> {
        let apps_path = ctx.apps_path();
        let install_path = apps_path
            .join(&self.name)
            .join("current")
            .join("install.json");

        debug!("Getting install manifest for {}", install_path.display());

        InstallManifest::from_path(install_path)
    }
}

/// Check if the manifest path is installed, and optionally confirm the bucket
///
/// # Panics
/// - The file was not valid UTF-8
pub fn is_installed(
    ctx: &impl ScoopContext<config::Scoop>,
    manifest_name: impl AsRef<Path>,
    bucket: Option<impl AsRef<str>>,
) -> bool {
    let install_path = ctx
        .apps_path()
        .join(manifest_name)
        .join("current/install.json");

    match InstallManifest::from_path(install_path) {
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

/// Merge defaults for a given architecture and the provided field
pub trait MergeDefaults {
    /// Output & Input type
    type Default;

    /// Merge the architecture specific autoupdate config with the arch agnostic one
    fn merge_default(&self, default: Self::Default, arch: Architecture) -> Self::Default;
}

impl MergeDefaults for Option<AutoupdateArchitecture> {
    type Default = AutoupdateConfig;

    #[must_use]
    /// Merge the architecture specific autoupdate config with the arch agnostic one
    fn merge_default(&self, default: Self::Default, arch: Architecture) -> Self::Default {
        let Some(config) = self
            .as_ref()
            .and_then(|config| arch_config!(config.arch => clone))
        else {
            return default;
        };

        AutoupdateConfig {
            bin: config.bin.or(default.bin),
            env_add_path: config.env_add_path.or(default.env_add_path),
            env_set: config.env_set.or(default.env_set),
            extract_dir: config.extract_dir.or(default.extract_dir),
            hash: config.hash.or(default.hash),
            installer: config.installer.or(default.installer),
            shortcuts: config.shortcuts.or(default.shortcuts),
            url: config.url.or(default.url),
        }
    }
}

impl MergeDefaults for Option<&ManifestArchitecture> {
    type Default = InstallConfig;

    #[allow(deprecated)]
    #[must_use]
    /// Merge the architecture specific autoupdate config with the arch agnostic one
    fn merge_default(&self, default: Self::Default, arch: Architecture) -> Self::Default {
        let Some(config) = self
            .as_ref()
            .and_then(|config| arch_config!(config.arch => clone))
        else {
            return default;
        };

        InstallConfig {
            bin: config.bin.or(default.bin),
            checkver: config.checkver.or(default.checkver),
            extract_dir: config.extract_dir.or(default.extract_dir),
            hash: config.hash.or(default.hash),
            installer: config.installer.or(default.installer),
            msi: config.msi.or(default.msi),
            post_install: config.post_install.or(default.post_install),
            post_uninstall: config.post_uninstall.or(default.post_uninstall),
            pre_install: config.pre_install.or(default.pre_install),
            pre_uninstall: config.pre_uninstall.or(default.pre_uninstall),
            shortcuts: config.shortcuts.or(default.shortcuts),
            uninstaller: config.uninstaller.or(default.uninstaller),
            url: config.url.or(default.url),
        }
    }
}

impl MergeDefaults for Option<ManifestArchitecture> {
    type Default = InstallConfig;

    #[allow(deprecated)]
    #[must_use]
    /// Merge the architecture specific autoupdate config with the arch agnostic one
    fn merge_default(&self, default: Self::Default, arch: Architecture) -> Self::Default {
        self.as_ref().merge_default(default, arch)
    }
}

impl HashExtractionOrArrayOfHashExtractions {
    #[must_use]
    /// Get the hash extraction as a single hash extraction object
    pub fn as_object(&self) -> Option<&HashExtraction> {
        match self {
            Self::Url(_) => None,
            Self::HashExtraction(hash) => Some(hash),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use crate::{buckets::Bucket, contexts::User, Architecture};
    use rayon::prelude::*;

    #[test]
    fn test_parse_all_manifests() -> Result<(), Box<dyn Error>> {
        const UNSUPPORTED_PACKAGES: &[&str] = &["unityhub"];

        let buckets = Bucket::list_all(&User::new())?;

        let manifests = buckets
            .into_par_iter()
            .flat_map(|bucket| bucket.list_packages())
            .flatten()
            .filter(|manifest| !UNSUPPORTED_PACKAGES.contains(&manifest.name.as_str()))
            .filter(|manifest| manifest.autoupdate_config(Architecture::ARCH).is_some())
            .collect::<Vec<_>>();

        manifests.par_iter().for_each(|manifest| {
            assert!(!manifest.name.is_empty());
            assert!(!manifest.bucket.is_empty());

            if let Some(autoupdate_config) = &manifest.autoupdate_config(Architecture::ARCH) {
                assert!(
                    autoupdate_config.url.is_some(),
                    "URL is missing in package: {}",
                    manifest.name
                );
            }
        });

        Ok(())
    }
}
