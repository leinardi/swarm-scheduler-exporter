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

//! TLS from `DOCKER_CERT_PATH`, as go-connections' `tlsconfig.Client` builds it for the Go
//! exporter, with Q5: verification is never off.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};

use super::error::ConfigError;
use crate::gofmt_compat::errno::io_error_text;
use crate::gofmt_compat::quote::quote;

/// A file that could not be read: Go's `*fs.PathError` text (`open <path>: <errno>`), with the
/// `io::Error` as its source.
#[derive(Debug)]
pub struct PathError {
    path: PathBuf,
    error: io::Error,
}

impl PathError {
    /// The path that could not be read.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "open {}: {}",
            self.path.display(),
            io_error_text(&self.error)
        )
    }
}

impl std::error::Error for PathError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

fn read(path: PathBuf) -> Result<Vec<u8>, PathError> {
    std::fs::read(&path).map_err(|error| PathError { path, error })
}

/// The TLS configuration for `cert_path`: the system roots plus `ca.pem`, the client certificate
/// `cert.pem` with its key `key.pem`, TLS 1.2 or 1.3, and no ALPN (HTTP/1 only, as Go's empty
/// `TLSNextProto`). `verify` is whether `DOCKER_TLS_VERIFY` is non-empty.
///
/// Q5: Go turns server verification off when `DOCKER_TLS_VERIFY` is empty; here that refuses to
/// build, so the exporter stops at startup with the fix in the message.
pub(crate) fn client_config(cert_path: &str, verify: bool) -> Result<ClientConfig, ConfigError> {
    let context = format!("configure TLS from DOCKER_CERT_PATH={cert_path}");
    if !verify {
        return Err(ConfigError::new(format!(
            "{context}: DOCKER_TLS_VERIFY is empty, which would connect without verifying the \
             daemon's certificate; set DOCKER_TLS_VERIFY=1"
        )));
    }
    build(Path::new(cert_path)).map_err(|error| error.context(&context))
}

fn build(cert_path: &Path) -> Result<ClientConfig, ConfigError> {
    let roots = cert_pool(&cert_path.join("ca.pem"))?;
    let (chain, key) = client_certificate(&cert_path.join("cert.pem"), &cert_path.join("key.pem"))
        .map_err(|error| error.context("could not load X509 key pair"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|error| ConfigError::new(error.to_string()))?
        .with_root_certificates(roots)
        .with_client_auth_cert(chain, key)
        .map_err(|error| {
            ConfigError::new(format!(
                "could not load X509 key pair: {}",
                key_pair_error(&error)
            ))
        })
}

/// go-connections' `certPool`: the system pool (Go's `x509.SystemCertPool`, which never fails on
/// Linux and is empty without roots) plus every certificate of `ca_file`, of which at least one
/// must parse.
pub(super) fn cert_pool(ca_file: &Path) -> Result<RootCertStore, ConfigError> {
    let mut roots = RootCertStore::empty();
    let system = rustls_native_certs::load_native_certs();
    roots.add_parsable_certificates(system.certs);
    let pem = read(ca_file.to_owned()).map_err(|error| {
        ConfigError::with_source(
            format!(
                "could not read CA certificate {}: {error}",
                quote(&ca_file.display().to_string())
            ),
            error,
        )
    })?;
    // Go's AppendCertsFromPEM skips a block that is not a certificate or does not parse.
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&pem)
        .filter_map(Result::ok)
        .collect();
    let (added, _) = roots.add_parsable_certificates(certs);
    if added == 0 {
        return Err(ConfigError::new(format!(
            "failed to append certificates from PEM file: {}",
            quote(&ca_file.display().to_string())
        )));
    }
    Ok(roots)
}

/// go-connections' `getCert`: both files read, the key's first PEM block not encrypted, then the
/// pair parsed as `tls.X509KeyPair` does.
fn client_certificate(
    cert_file: &Path,
    key_file: &Path,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>), ConfigError> {
    let cert_pem = read(cert_file.to_owned())
        .map_err(|error| ConfigError::with_source(error.to_string(), error))?;
    let key_pem = read(key_file.to_owned())
        .map_err(|error| ConfigError::with_source(error.to_string(), error))?;
    check_private_key(&key_pem)?;
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_pem)
        .filter_map(Result::ok)
        .collect();
    if chain.is_empty() {
        return Err(ConfigError::new(
            "tls: failed to find any PEM data in certificate input".to_owned(),
        ));
    }
    // Go takes the first block whose type ends in PRIVATE KEY; an encrypted PKCS #8 one is such a
    // block, and then fails to parse.
    let key = PrivateKeyDer::from_pem_slice(&key_pem).map_err(|_| {
        let encrypted_pkcs8 = pem_types(&key_pem)
            .into_iter()
            .find(|kind| kind.ends_with("PRIVATE KEY"))
            .is_some_and(|kind| kind == "ENCRYPTED PRIVATE KEY");
        ConfigError::new(if encrypted_pkcs8 {
            "tls: failed to parse private key".to_owned()
        } else {
            "tls: failed to find any PEM data in key input".to_owned()
        })
    })?;
    Ok((chain, key))
}

/// go-connections' `getPrivateKey`: the key file must hold a PEM block, and its first block must
/// not be a legacy encrypted one (a `DEK-Info` header, Go's `x509.IsEncryptedPEMBlock`).
fn check_private_key(key_pem: &[u8]) -> Result<(), ConfigError> {
    let text = String::from_utf8_lossy(key_pem);
    let encrypted = text
        .lines()
        .skip_while(|line| !line.starts_with("-----BEGIN "))
        .skip(1)
        .take_while(|line| line.contains(':'))
        .any(|line| line.starts_with("DEK-Info:"));
    if encrypted {
        return Err(ConfigError::new(
            "private key is encrypted; encrypted private keys are obsolete, and not supported"
                .to_owned(),
        ));
    }
    if pem_types(key_pem).is_empty() {
        return Err(ConfigError::new("no valid private key found".to_owned()));
    }
    Ok(())
}

/// The types of the PEM blocks in `pem`, in order: what follows `-----BEGIN ` on each line that
/// opens a block.
fn pem_types(pem: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(pem)
        .lines()
        .filter_map(|line| {
            line.trim_end()
                .strip_prefix("-----BEGIN ")?
                .strip_suffix("-----")
        })
        .map(str::to_owned)
        .collect()
}

/// `tls.X509KeyPair`'s text for a key rustls refuses.
fn key_pair_error(error: &rustls::Error) -> String {
    if matches!(error, rustls::Error::InconsistentKeys(_)) {
        "tls: private key does not match public key".to_owned()
    } else {
        "tls: failed to parse private key".to_owned()
    }
}
