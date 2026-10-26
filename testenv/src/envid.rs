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

//! The run's envid, the label that namespaces everything it creates, and the resource names
//! derived from it.

use std::fmt::Write as _;
use std::fs::File;
use std::io::Read as _;

use crate::Error;

/// Four random bytes, eight hex characters.
const ENV_ID_BYTES: usize = 4;

/// The Docker label key applied to every resource the harness creates. Use it with
/// [`label_filter`] to scope Docker API queries to one run, or on its own to find every run.
pub const LABEL_ENV_ID: &str = "swarm-scheduler-exporter.it.envid";

const RESOURCE_PREFIX: &str = "sse-it";

/// A fresh envid: eight lowercase hex characters from the kernel's random source, as Go's
/// `crypto/rand` reads it.
///
/// # Errors
///
/// The random source cannot be read.
pub fn generate_env_id() -> Result<String, Error> {
    let mut bytes = [0_u8; ENV_ID_BYTES];
    File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| Error::new(format!("generate env ID: {error}")))?;
    Ok(bytes.iter().fold(String::new(), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
    }))
}

/// The name of the run's resource `base`: `sse-it-<envid>-<base>`.
#[must_use]
pub fn resource_name(env_id: &str, base: &str) -> String {
    format!("{RESOURCE_PREFIX}-{env_id}-{base}")
}

/// The `label` filter value that selects the run's resources.
#[must_use]
pub fn label_filter(env_id: &str) -> String {
    format!("{LABEL_ENV_ID}={env_id}")
}
