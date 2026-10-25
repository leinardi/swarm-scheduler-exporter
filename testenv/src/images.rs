/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

//! The workload image: pulled once on the host, saved to one archive, and loaded from it into
//! every node.

use bollard::Docker;
use bollard::errors::Error as DockerError;
use bollard::query_parameters::ImportImageOptions;
use futures_util::StreamExt as _;
use swarm_scheduler_exporter::logger::Attr;
use tempfile::TempPath;
use tokio::io::AsyncWriteExt as _;
use tokio_util::io::ReaderStream;

use crate::Error;
use crate::cluster::Node;
use crate::context::Context;
use crate::dind::{IMAGE_TRANSFER_TIMEOUT, OP_TIMEOUT, OpError, ensure_host_image, with_timeout};
use crate::redact::RedactingLogger;

/// The one image every test service runs. It is digest-pinned so every run loads the same
/// bytes, and it is pulled only on the host: the nodes receive it through an image load and
/// services reference it by image ID, which makes Swarm's executor skip the pull entirely. The
/// nodes never contact a registry, so there are no rate limits or network flakes inside the
/// cluster.
pub const WORKLOAD_IMAGE: &str =
    "busybox:1.37.0@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e";

const LOADED_IMAGE_ID_PREFIX: &str = "Loaded image ID: ";
const LOADED_IMAGE_REF_PREFIX: &str = "Loaded image: ";

/// Copies [`WORKLOAD_IMAGE`] from the host to every node and returns its image ID, which must be
/// the same on every node.
pub(crate) async fn load_workload_image(
    ctx: &Context,
    host_client: &Docker,
    nodes: &[Node],
    log: &RedactingLogger,
) -> Result<String, Error> {
    ensure_host_image(ctx, host_client, WORKLOAD_IMAGE, log).await?;

    // Removed when dropped, on every path.
    let archive = save_image_to_file(ctx, host_client, WORKLOAD_IMAGE).await?;

    let mut image_id = String::new();
    for node in nodes {
        let node_image_id = load_image_on_node(ctx, node, &archive).await?;
        if !image_id.is_empty() && node_image_id != image_id {
            return Err(Error::new(format!(
                "{} has {image_id}, {} has {node_image_id}: workload image ID differs between \
                 nodes",
                nodes[0].name, node.name
            )));
        }
        image_id = node_image_id;
    }

    log.info(
        "workload image loaded on every node",
        &[
            Attr::string("image", WORKLOAD_IMAGE),
            Attr::string("id", image_id.clone()),
        ],
    );
    Ok(image_id)
}

/// Writes the image archive to a temporary file, so each node can be fed a fresh reader instead
/// of the whole archive being held in memory. The file is removed when the path is dropped.
async fn save_image_to_file(
    ctx: &Context,
    host_client: &Docker,
    image_ref: &str,
) -> Result<TempPath, Error> {
    let archive = tempfile::Builder::new()
        .prefix("sse-it-workload-")
        .suffix(".tar")
        .tempfile()
        .map_err(|error| Error::new(format!("create image archive: {error}")))?;
    let (file, path) = archive.into_parts();
    let mut file = tokio::fs::File::from_std(file);

    let save = async {
        let mut saved = host_client.export_images(&[image_ref]);
        while let Some(chunk) = saved.next().await {
            let chunk = chunk.map_err(|error| format!("save host image {image_ref:?}: {error}"))?;
            file.write_all(&chunk)
                .await
                .map_err(|error| format!("write image archive for {image_ref:?}: {error}"))?;
        }
        file.flush()
            .await
            .map_err(|error| format!("close image archive: {error}"))
    };
    match ctx.with_timeout(IMAGE_TRANSFER_TIMEOUT).run(save).await {
        Ok(Ok(())) => Ok(path),
        Ok(Err(message)) => Err(Error::new(message)),
        Err(ended) => Err(Error::new(format!(
            "save host image {image_ref:?}: {ended}"
        ))),
    }
}

/// Loads the archive into the node's daemon and returns the loaded image's ID, as confirmed by
/// an image inspect on that node.
async fn load_image_on_node(
    ctx: &Context,
    node: &Node,
    archive: &TempPath,
) -> Result<String, Error> {
    let on_node = |error: Error| error.context(format!("load workload image on {:?}", node.name));

    let file = tokio::fs::File::open(archive)
        .await
        .map_err(|error| Error::new(format!("open image archive: {error}")))?;
    let client = node.client().map_err(on_node)?;

    let loaded_ref = load_archive(ctx, &client, file).await.map_err(on_node)?;

    let inspected = with_timeout(ctx, OP_TIMEOUT, client.inspect_image(&loaded_ref))
        .await
        .map_err(|error| {
            on_node(Error::new(format!(
                "inspect loaded image {loaded_ref:?}: {error}"
            )))
        })?;
    Ok(inspected.id.unwrap_or_default())
}

/// Sends the archive to the daemon and reads its whole response: the load is only complete once
/// the daemon has written all of it. Returns the reference the daemon reports as loaded,
/// preferring the image ID over a tag when it reports both.
async fn load_archive(
    ctx: &Context,
    client: &Docker,
    archive: tokio::fs::File,
) -> Result<String, Error> {
    let load = async {
        let mut messages = client.import_image_stream(
            ImportImageOptions {
                quiet: true,
                ..ImportImageOptions::default()
            },
            ReaderStream::new(archive),
            None,
        );
        let mut loaded = Loaded::default();
        while let Some(message) = messages.next().await {
            match message {
                Ok(info) => loaded.observe(info.stream.as_deref().unwrap_or_default()),
                // bollard turns a message's errorDetail into this error.
                Err(DockerError::DockerStreamError { error }) => {
                    return Ok(Err(Error::new(format!(
                        "image load response: workload image load reported an error: {error}"
                    ))));
                }
                Err(error) => return Err(OpError::from(error)),
            }
        }
        Ok(loaded.finish())
    };
    with_timeout(ctx, IMAGE_TRANSFER_TIMEOUT, load)
        .await
        .map_err(|error| Error::new(format!("image load: {error}")))?
}

/// What an image load's messages report as loaded.
#[derive(Default)]
struct Loaded {
    id: String,
    reference: String,
}

impl Loaded {
    fn observe(&mut self, stream: &str) {
        let line = stream.trim();
        if let Some(id) = line.strip_prefix(LOADED_IMAGE_ID_PREFIX) {
            id.clone_into(&mut self.id);
        } else if let Some(reference) = line.strip_prefix(LOADED_IMAGE_REF_PREFIX) {
            reference.clone_into(&mut self.reference);
        }
    }

    fn finish(self) -> Result<String, Error> {
        if !self.id.is_empty() {
            return Ok(self.id);
        }
        if !self.reference.is_empty() {
            return Ok(self.reference);
        }
        Err(Error::new("image load response: workload image not loaded"))
    }
}
