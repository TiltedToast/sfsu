//! Cache helpers

use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    ops::Deref,
    path::{Path, PathBuf},
};

use bytes::BytesMut;
use digest::Digest;
use futures::{Stream, StreamExt, TryStreamExt};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use reqwest::{Response, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::codec::{BytesCodec, FramedRead};

use crate::{
    hash::{url_ext::UrlExt, HashType},
    packages::Manifest,
};

#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
/// Cache error
pub enum Error {
    #[error("Failed to download file: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Failed to write to file: {0}")]
    IO(#[from] std::io::Error),
    #[error("HTTP Error: {0}")]
    ErrorCode(StatusCode),
    #[error("Missing download url in manifest")]
    MissingDownloadUrl,
}

#[derive(Debug)]
/// A cache handle
pub struct Handle {
    url: String,
    /// The cache output file name
    pub file_name: PathBuf,
    /// The cache output file path
    cache_path: PathBuf,
    hash_type: HashType,
}

impl Handle {
    /// Construct a new cache handle
    ///
    /// # Errors
    /// - If the file cannot be created
    pub fn new(
        cache_path: impl AsRef<Path>,
        file_name: impl Into<PathBuf>,
        hash_type: HashType,
        url: String,
    ) -> Result<Self, Error> {
        let file_name = file_name.into();
        let cache_path = cache_path.as_ref().join(&file_name);
        Ok(Self {
            url,
            file_name,
            cache_path,
            hash_type,
        })
    }

    /// Open a manifest and return a cache handle
    ///
    /// # Errors
    /// - IO errors
    /// - Missing download URL
    pub fn open_manifest(cache_path: impl AsRef<Path>, manifest: &Manifest) -> Result<Self, Error> {
        let name = &manifest.name;
        let version = &manifest.version;

        let url = manifest.download_url().ok_or(Error::MissingDownloadUrl)?;

        let file_name = PathBuf::from(&url);

        let file_name = format!("{}#{}#{}", name, version, file_name.display());

        Self::new(
            cache_path.as_ref(),
            PathBuf::from(file_name),
            HashType::default(),
            url.url,
        )
    }

    /// Create a new downloader
    ///
    /// # Errors
    /// - If the request fails
    pub async fn begin_download(
        self,
        client: &impl Deref<Target = reqwest::Client>,
        mp: Option<&MultiProgress>,
    ) -> Result<Downloader, Error> {
        Downloader::new(self, client, mp).await
    }
}

#[derive(Debug)]
#[must_use = "Does nothing until `download` is called"]
/// A cache handle downloader
pub struct Downloader {
    cache: Handle,
    resp: Response,
    pb: Option<ProgressBar>,
    message: &'static str,
}

impl Downloader {
    /// Create a new downloader
    ///
    /// # Errors
    /// - If the request fails
    ///
    /// # Panics
    /// - A non-empty file name
    /// - Invalid progress style template
    pub async fn new(
        cache: Handle,
        client: &impl Deref<Target = reqwest::Client>,
        mp: Option<&MultiProgress>,
    ) -> Result<Self, Error> {
        let resp = client.get(&cache.url).send().await?;

        if !resp.status().is_success() {
            return Err(Error::ErrorCode(resp.status()));
        }

        debug!("Status Code: {}", resp.status());

        let content_length = resp.content_length().unwrap_or_default();

        let (pb, message) = mp.map_or((None, ""), |mp| {
            let boxed = {
                if let Ok(parsed_url) = url::Url::parse(&cache.url)
                    && let Some(leaf) = parsed_url.leaf()
                {
                    leaf.into_boxed_str()
                } else {
                    cache
                        .file_name
                        .to_string_lossy()
                        .split('_')
                        .next_back()
                        .expect("non-empty file name")
                        .to_string()
                        .into_boxed_str()
                }
            };

            let message: &'static str = Box::leak(boxed);

                let pb = mp.add(
                ProgressBar::new(content_length)
                    .with_style(
                        ProgressStyle::with_template(
                            "{prefix} {msg} {spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} {bytes_per_sec} ({eta})",
                        )
                        .unwrap()
                        .progress_chars("#>-"),
                    )
                    .with_message(message)
                    .with_finish(indicatif::ProgressFinish::WithMessage("Finished ✅".into())),
            );

            (Some(pb),message)
        });

        Ok(Self {
            cache,
            resp,
            pb,
            message,
        })
    }

    /// Download the file to the cache
    ///
    /// Returns the cache file name, and the computed hash
    ///
    /// # Errors
    /// - If the file cannot be written to the cache
    pub async fn download(self) -> Result<(PathBuf, Vec<u8>), Error> {
        let file_name = self.cache.file_name.clone();
        let hash_bytes = match self.cache.hash_type {
            HashType::SHA512 => self.handle_buf::<sha2::Sha512>().await,
            HashType::SHA256 => self.handle_buf::<sha2::Sha256>().await,
            HashType::SHA1 => self.handle_buf::<sha1::Sha1>().await,
            HashType::MD5 => self.handle_buf::<md5::Md5>().await,
        }?;

        // Hash;

        // loop {
        //     let chunk = reader.fill_buf()?;
        //     let chunk_length = chunk.len();

        //     if chunk_length == 0 {
        //         break;
        //     }

        //     self.cache.write_all(chunk)?;
        //     self.pb.inc(chunk_length as u64);

        //     reader.consume(chunk_length);
        // }

        Ok((file_name, hash_bytes))
    }

    async fn handle_buf<D: Digest>(self) -> Result<Vec<u8>, Error> {
        use tokio::fs::File;

        enum Source<
            T: futures_core::Stream<Item = reqwest::Result<bytes::Bytes>> + std::marker::Unpin,
        > {
            Cache(futures::prelude::stream::IntoStream<FramedRead<File, BytesCodec>>),
            Network(T),
        }

        impl<T> Stream for Source<T>
        where
            T: futures_core::Stream<Item = reqwest::Result<bytes::Bytes>> + std::marker::Unpin,
        {
            type Item = reqwest::Result<bytes::Bytes>;

            fn poll_next(
                self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Self::Item>> {
                match self.get_mut() {
                    Source::Cache(file) => {
                        std::pin::pin!(file).poll_next(cx).map(|bytes| match bytes {
                            Some(Ok(bytes)) => Some(Ok(BytesMut::freeze(bytes))),
                            _ => None,
                        })
                    }
                    Source::Network(resp) => resp.poll_next_unpin(cx),
                }
            }
        }

        // impl<'a> tokio_stream::Stream for Source<'a> {
        //     type Item = bytes::Bytes;

        //     fn poll_next(
        //         self: std::pin::Pin<&mut Self>,
        //         cx: &mut std::task::Context<'_>,
        //     ) -> std::task::Poll<Option<Self::Item>> {
        //         match self {
        //             Source::Cache(file) => {
        //                 let mut buf = vec![];

        //                 file.read(&mut buf).then(|buf_length| async {
        //                     if buf.is_empty() {
        //                         None
        //                     } else {
        //                         Some(buf)
        //                     }
        //                 });
        //             }
        //             Source::Network(resp) => todo!(),
        //         }
        //         todo!()
        //     }
        // }
        // impl<'a> Read for Source<'a> {
        //     fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        //         match self {
        //             Source::Cache(file) => file.read(buf),
        //             Source::Network(resp) => resp.read(buf),
        //         }
        //     }
        // }
        let cache_path = self.cache.cache_path.clone();

        let mut reader = if cache_path.exists() {
            debug!("Loading from cache");
            if let Some(pb) = &self.pb {
                pb.set_prefix("📦");
            }
            let file = File::open(&cache_path).await?;
            let stream = FramedRead::new(file, BytesCodec::new());

            Source::Cache(stream.into_stream())
        } else {
            debug!("Downloading via network");
            Source::Network(self.resp.bytes_stream())
        };

        let mut cache_file = match &reader {
            Source::Cache(_) => None,
            Source::Network(_) => Some(File::create(&cache_path).await?),
        };

        let mut hasher = D::new();

        while let Some(Ok(chunk)) = reader.next().await {
            hasher.update(&chunk);

            if let Some(cache_file) = cache_file.as_mut() {
                cache_file.write_all(&chunk).await?;
            }

            let chunk_length = chunk.len();

            if let Some(pb) = &self.pb {
                pb.inc(chunk_length as u64);
            }
        }

        Ok(hasher.finalize()[..].to_vec())
    }
}

// impl Drop for Downloader {
//     fn drop(&mut self) {
//         // There is no code that would drop this message
//         // As such this should be safe
//         unsafe { core::ptr::drop_in_place(std::ptr::from_ref::<str>(self.message).cast_mut()) };
//     }
// }
