use clap::Parser;

use sfsu::buckets::Bucket;

#[derive(Debug, Clone, Parser)]
/// Describe a package
pub struct Args {
    #[clap(help = "The package to describe")]
    package: String,

    #[clap(short, long, help = "The bucket to exclusively search in")]
    bucket: Option<String>,
}

impl super::Command for Args {
    fn run(self) -> Result<(), anyhow::Error> {
        let manifests = if let Some(bucket_name) = self.bucket {
            let bucket = Bucket::new(&bucket_name);

            vec![(
                self.package.clone(),
                bucket_name,
                bucket.get_manifest(&self.package)?,
            )]
        } else {
            let buckets = Bucket::list_all()?;

            buckets
                .iter()
                .filter_map(|bucket| match bucket.get_manifest(&self.package) {
                    Ok(manifest) => Some((self.package.clone(), bucket.name.clone(), manifest)),
                    Err(_) => None,
                })
                .collect()
        };

        for (package, bucket, manifest) in manifests {
            println!("{package} in \"{bucket}\":");

            if let Some(description) = manifest.description {
                println!("  {description}");
            }

            println!("  Version: {}", manifest.version);

            if let Some(homepage) = manifest.homepage {
                println!("  Homepage: {homepage}");
            }
            if let Some(license) = manifest.license {
                println!("  License: {license}");
            }
        }

        Ok(())
    }
}
