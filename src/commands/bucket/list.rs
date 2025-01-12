use anyhow::Context;
use chrono::{DateTime, FixedOffset};
use clap::Parser;
use serde::Serialize;
use sprinkles::{
    buckets::Bucket,
    config,
    contexts::ScoopContext,
    output::{self, wrappers::time::NicerTime},
};
use tokio::task::JoinSet;

#[derive(Debug, Clone, Parser)]
pub struct Args {
    #[clap(from_global)]
    json: bool,
}

#[derive(Debug, Clone, Serialize)]
struct BucketInfo {
    name: String,
    source: String,
    updated: NicerTime<DateTime<FixedOffset>>,
    manifests: usize,
}

impl BucketInfo {
    async fn collect(bucket: Bucket) -> anyhow::Result<Self> {
        let manifests = bucket.manifests_async().await?;

        let updated_time = {
            let repo = bucket.open_repo()?;
            let latest_commit = repo.latest_commit()?;
            let time = sprinkles::git::parity::Time::from(latest_commit.time());

            time.to_datetime().context("invalid time")?
        };

        Ok(Self {
            name: bucket.name().to_string(),
            source: bucket.source()?.to_string(),
            updated: updated_time.into(),
            manifests,
        })
    }
}

impl super::Command for Args {
    async fn runner(self, ctx: &impl ScoopContext<config::Scoop>) -> anyhow::Result<()> {
        let buckets = Bucket::list_all(ctx)?;

        let mut set = JoinSet::new();

        for bucket in buckets {
            set.spawn(BucketInfo::collect(bucket));
        }

        let buckets = {
            let mut buckets = vec![];

            while let Some(result) = set.join_next().await {
                let result = result??;
                buckets.push(result);
            }

            buckets.sort_by(|a, b| a.name.cmp(&b.name));

            buckets
        };

        if self.json {
            let output = serde_json::to_string_pretty(&buckets)?;
            println!("{output}");
        } else {
            let structured = output::structured::Structured::new(&buckets);

            println!("{structured}");
        }

        Ok(())
    }
}
