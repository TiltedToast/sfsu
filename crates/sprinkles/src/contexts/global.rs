use std::{
    fs::File,
    path::{Path, PathBuf},
};

use crate::{config, git};

use super::{ScoopContext, User};

#[derive(Debug, Clone)]
/// Global context adapter
pub struct Global {
    path: PathBuf,
    user_context: User,
}

impl Default for Global {
    fn default() -> Self {
        Self::new()
    }
}

impl Global {
    #[must_use]
    /// Construct a new global context adapter
    pub fn new() -> Self {
        use std::env::var_os;

        let user_context = User::new();

        let path = {
            if let Some(path) = var_os("SCOOP_GLOBAL") {
                path.into()
            } else {
                user_context.config().global_path.clone()
            }
        };

        let path = if path.exists() {
            dunce::canonicalize(path).expect("failed to find real path to scoop")
        } else {
            panic!("Scoop path does not exist");
        };

        Self { path, user_context }
    }
}

impl ScoopContext<config::Scoop> for Global {
    const CONTEXT_NAME: &'static str = User::CONTEXT_NAME;

    fn config(&self) -> &config::Scoop {
        self.user_context.config()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn git_path() -> Result<PathBuf, which::Error> {
        which::which("git")
    }

    fn apps_path(&self) -> PathBuf {
        self.sub_path("apps")
    }

    fn buckets_path(&self) -> PathBuf {
        self.user_context.buckets_path()
    }

    fn cache_path(&self) -> PathBuf {
        self.user_context.cache_path()
    }

    fn persist_path(&self) -> PathBuf {
        self.user_context.persist_path()
    }

    fn shims_path(&self) -> PathBuf {
        self.sub_path("shims")
    }

    fn workspace_path(&self) -> PathBuf {
        self.user_context.workspace_path()
    }

    fn logging_dir(&self) -> std::io::Result<PathBuf> {
        self.user_context.logging_dir()
    }

    #[allow(deprecated)]
    async fn new_log(&self) -> Result<File, super::Error> {
        self.user_context.new_log().await
    }

    #[allow(deprecated)]
    fn new_log_sync(&self) -> Result<File, super::Error> {
        self.user_context.new_log_sync()
    }

    fn open_repo(&self) -> Option<git::Result<git::Repo>> {
        self.user_context.open_repo()
    }

    fn context_app_path(&self) -> PathBuf {
        self.user_context.context_app_path()
    }

    async fn outdated(&self) -> Result<bool, super::Error> {
        self.user_context.outdated().await
    }
}
