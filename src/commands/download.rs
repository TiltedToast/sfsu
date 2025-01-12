use clap::Parser;
use indicatif::MultiProgress;

use sprinkles::{
    cache::{Downloader, Handle},
    config,
    contexts::ScoopContext,
    packages::reference::Package,
    requests::AsyncClient,
    Architecture,
};

use crate::{abandon, output::colours::eprintln_yellow};

#[derive(Debug, Clone, Parser)]
// TODO: Pass architecture
pub struct Args {
    #[clap(short, long, help = "Use the specified architecture, if the app supports it", default_value_t = Architecture::ARCH)]
    arch: Architecture,

    #[clap(short = 'H', long, help = "Disable hash validation")]
    no_hash_check: bool,

    #[clap(help = "The packages to download")]
    packages: Vec<Package>,

    #[clap(from_global)]
    json: bool,
}

impl super::Command for Args {
    const BETA: bool = true;

    async fn runner(self, ctx: &impl ScoopContext<config::Scoop>) -> Result<(), anyhow::Error> {
        if self.packages.is_empty() {
            abandon!("No packages provided")
        }

        if self.no_hash_check {
            eprintln_yellow!(
                "Hash check has been disabled! This may allow modified files to be downloaded"
            );
        }

        let mp = MultiProgress::new();

        eprint!("Attempting to generate manifest(s)");
        let downloaders: Vec<Downloader> =
            futures::future::try_join_all(self.packages.into_iter().map(|package| {
                let mp = mp.clone();
                async move {
                    let manifest = match package.manifest(ctx).await {
                        Ok(manifest) => manifest,
                        Err(e) => abandon!("\rFailed to generate manifest: {e}"),
                    };

                    let dl = Handle::open_manifest(ctx.cache_path(), &manifest, self.arch)?;

                    let downloaders = dl.into_iter().map(|dl| {
                        let mp = mp.clone();
                        async move {
                            match Downloader::new::<AsyncClient>(dl, Some(&mp)).await {
                                Ok(dl) => anyhow::Ok(dl),
                                Err(e) => match e {
                                    sprinkles::cache::Error::ErrorCode(status) => {
                                        abandon!("Found {status} error while downloading")
                                    }
                                    _ => Err(e.into()),
                                },
                            }
                        }
                    });
                    let downloaders = futures::future::try_join_all(downloaders).await?;

                    anyhow::Ok(downloaders)
                }
            }))
            .await?
            .into_iter()
            .flatten()
            .collect();
        eprintln!("\r📜 Generated manifest for any and all mismatched versions");

        let threads = downloaders
            .into_iter()
            .map(|dl| tokio::spawn(async move { dl.download().await }));

        let results = futures::future::try_join_all(threads).await?;

        for result in results {
            let result = result?;

            if !self.no_hash_check {
                eprint!("🔓 Checking {} hash...", result.file_name.url);

                let actual_hash = result.actual_hash.no_prefix();

                if result.actual_hash == result.computed_hash {
                    eprintln!("\r🔒 Hash matched: {actual_hash}");
                } else {
                    eprintln!();
                    abandon!(
                        "🔓 Hash mismatch: expected {actual_hash}, found {}",
                        result.computed_hash.no_prefix()
                    );
                }
                // } else {
                //     eprintln!();
                //     warn!("🔓 No hash provided, skipping hash check");
                // }
            }

            eprintln!("✅ Downloaded {}", result.file_name.url);
        }

        Ok(())
    }
}
