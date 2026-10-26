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

//! The suite's HTTP client, for the exporter's endpoints and the socket proxy: one request under
//! the caller's context and [`HTTP_TIMEOUT`], the whole body read.

use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper::{Method, Request};
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioExecutor;
use testenv::context::Context;
use testenv::metrics_text::Scrape;

/// Bounds one request to the exporter.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(5);

/// `GET url`: the status and the body.
pub async fn get(ctx: &Context, url: &str) -> Result<(u16, Vec<u8>), String> {
    request(ctx, Method::GET, url).await
}

/// `method url`: the status and the body.
pub async fn request(ctx: &Context, method: Method, url: &str) -> Result<(u16, Vec<u8>), String> {
    let client = Client::builder(TokioExecutor::new()).build_http::<Empty<Bytes>>();
    let request = Request::builder()
        .method(method.clone())
        .uri(url)
        .body(Empty::new())
        .map_err(|error| format!("build request for {url}: {error}"))?;
    let exchange = async {
        let response = client
            .request(request)
            .await
            .map_err(|error| format!("{method} {url}: {}", error_chain(&error)))?;
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|error| format!("read {url}: {error}"))?
            .to_bytes();
        Ok((status, body.to_vec()))
    };
    ctx.with_timeout(HTTP_TIMEOUT)
        .run(exchange)
        .await
        .map_err(|ended| format!("{method} {url}: {ended}"))?
}

/// Reads and parses `base_url/metrics`.
pub async fn scrape_metrics(ctx: &Context, base_url: &str) -> Result<Scrape, String> {
    let (status, body) = get(ctx, &format!("{base_url}/metrics")).await?;
    if status != 200 {
        return Err(format!("GET /metrics: unexpected HTTP status {status}"));
    }
    Scrape::parse(&String::from_utf8_lossy(&body))
        .map_err(|error| format!("parse /metrics: {error}"))
}

/// An error with its sources, as Go prints a wrapped one: hyper-util's own text shows one level.
pub fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}
