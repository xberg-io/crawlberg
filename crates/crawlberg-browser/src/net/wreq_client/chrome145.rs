// Copyright 2026 0x676e67 <gngppz@gmail.com>
// SPDX-License-Identifier: Apache-2.0
// ~keep Adapted from wreq-util 3.0.0-rc.14; see LICENSE-WREQ-UTIL and NOTICE.
// ~keep Only Chrome145/Linux is retained, avoiding unused vulnerable Zstd certificate codecs.

use std::io::{self, Write};

use wreq::header::{HeaderMap, HeaderValue};
use wreq::http2::{Http2Options, PseudoId, PseudoOrder, SettingId, SettingsOrder, StreamDependency, StreamId};
use wreq::tls::compress::{CertificateCompressionAlgorithm, CertificateCompressor, Codec};
use wreq::tls::{AlpsProtocol, TlsOptions, TlsVersion};

use super::STEALTH_USER_AGENT;

const CURVES: &str = "X25519MLKEM768:X25519:P-256:P-384";
const SIGNATURE_ALGORITHMS: &str = concat!(
    "ecdsa_secp256r1_sha256:rsa_pss_rsae_sha256:rsa_pkcs1_sha256:",
    "ecdsa_secp384r1_sha384:rsa_pss_rsae_sha384:rsa_pkcs1_sha384:",
    "rsa_pss_rsae_sha512:rsa_pkcs1_sha512",
);
const CIPHERS: &str = concat!(
    "TLS_AES_128_GCM_SHA256:TLS_AES_256_GCM_SHA384:TLS_CHACHA20_POLY1305_SHA256:",
    "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:",
    "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:",
    "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:",
    "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:",
    "TLS_RSA_WITH_AES_128_GCM_SHA256:TLS_RSA_WITH_AES_256_GCM_SHA384:",
    "TLS_RSA_WITH_AES_128_CBC_SHA:TLS_RSA_WITH_AES_256_CBC_SHA",
);
const CERTIFICATE_COMPRESSORS: &[&dyn CertificateCompressor] = &[&BrotliCompressor];

pub(super) fn emulation() -> wreq::Emulation {
    wreq::Emulation::builder()
        .tls_options(tls_options())
        .http2_options(http2_options())
        .headers(headers())
        // ~keep Chrome145 inherits the upstream v132 transport and connection-pool identity.
        .build(wreq::Group::new("v132"))
}

fn tls_options() -> TlsOptions {
    TlsOptions::builder()
        .grease_enabled(true)
        .enable_ocsp_stapling(true)
        .enable_signed_cert_timestamps(true)
        .curves_list(CURVES)
        .sigalgs_list(SIGNATURE_ALGORITHMS)
        .cipher_list(CIPHERS)
        .min_tls_version(TlsVersion::TLS_1_2)
        .max_tls_version(TlsVersion::TLS_1_3)
        .permute_extensions(true)
        .pre_shared_key(true)
        .enable_ech_grease(true)
        .alps_protocols([AlpsProtocol::HTTP2])
        .alps_use_new_codepoint(true)
        .aes_hw_override(true)
        .certificate_compressors(CERTIFICATE_COMPRESSORS)
        .build()
}

fn http2_options() -> Http2Options {
    Http2Options::builder()
        .initial_window_size(6291456)
        .initial_connection_window_size(15728640)
        .max_header_list_size(262144)
        .header_table_size(65536)
        .headers_stream_dependency(StreamDependency::new(StreamId::zero(), 219, true))
        .headers_pseudo_order(
            PseudoOrder::builder()
                .extend([PseudoId::Method, PseudoId::Authority, PseudoId::Scheme, PseudoId::Path])
                .build(),
        )
        .settings_order(
            SettingsOrder::builder()
                .extend([
                    SettingId::HeaderTableSize,
                    SettingId::EnablePush,
                    SettingId::MaxConcurrentStreams,
                    SettingId::InitialWindowSize,
                    SettingId::MaxFrameSize,
                    SettingId::MaxHeaderListSize,
                    SettingId::EnableConnectProtocol,
                    SettingId::NoRfc7540Priorities,
                ])
                .build(),
        )
        .enable_push(false)
        .build()
}

fn headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in [
        (
            "sec-ch-ua",
            r#""Not:A-Brand";v="99", "Google Chrome";v="145", "Chromium";v="145""#,
        ),
        ("sec-ch-ua-mobile", "?0"),
        ("sec-ch-ua-platform", "\"Linux\""),
        ("user-agent", STEALTH_USER_AGENT),
        ("sec-fetch-dest", "document"),
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-site", "none"),
        (
            "accept",
            concat!(
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,",
                "image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7",
            ),
        ),
        ("accept-language", "en-US,en;q=0.9"),
        ("priority", "u=0, i"),
    ] {
        headers.insert(name, HeaderValue::from_static(value));
    }
    headers
}

#[derive(Debug)]
struct BrotliCompressor;

impl CertificateCompressor for BrotliCompressor {
    fn compress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut writer = brotli::CompressorWriter::new(output, input.len(), 11, 32);
            writer.write_all(input)?;
            writer.flush()
        })
    }

    fn decompress(&self) -> Codec {
        Codec::Pointer(|input, output| {
            let mut reader = brotli::Decompressor::new(input, 4096);
            io::copy(&mut reader, output)?;
            Ok(())
        })
    }

    fn algorithm(&self) -> CertificateCompressionAlgorithm {
        CertificateCompressionAlgorithm::BROTLI
    }
}

#[cfg(test)]
#[path = "chrome145_tests.rs"]
mod tests;
