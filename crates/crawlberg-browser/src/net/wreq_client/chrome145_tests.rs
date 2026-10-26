use std::io::{self, Write};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wreq::tls::compress::{CertificateCompressionAlgorithm, Codec};

use super::emulation;

// ~keep Snapshot captured from published wreq-util 3.0.0-rc.14, Chrome145/Linux, with default features.
const UPSTREAM_PROFILE: &str = include_str!("chrome145_profile.txt");

#[test]
fn profile_preserves_chrome145_linux_transport_and_header_fingerprint() {
    let profile = emulation();
    assert_eq!(profile.headers.len(), 10);
    assert_eq!(format!("{profile:#?}\n"), UPSTREAM_PROFILE);
}

#[test]
fn profile_snapshot_detects_tls_http2_and_header_drift() {
    let profile = emulation();
    let mut tls_changed = profile.clone();
    tls_changed.tls_options.as_mut().expect("TLS profile").grease_enabled = Some(false);
    assert_ne!(format!("{tls_changed:#?}\n"), UPSTREAM_PROFILE);
    let mut http2_changed = profile.clone();
    http2_changed.http2_options = None;
    assert_ne!(format!("{http2_changed:#?}\n"), UPSTREAM_PROFILE);
    let mut headers_changed = profile;
    headers_changed.headers.remove("sec-ch-ua");
    assert_ne!(format!("{headers_changed:#?}\n"), UPSTREAM_PROFILE);
}

fn apply(codec: Codec, input: &[u8], output: &mut dyn Write) -> io::Result<()> {
    match codec {
        Codec::Pointer(function) => function(input, output),
        Codec::Dynamic(function) => function(input, output),
    }
}

#[test]
fn certificate_compression_round_trips_empty_and_binary_certificates() {
    let profile = emulation();
    let tls = profile.tls_options.expect("TLS profile");
    let compressors = tls.certificate_compressors.expect("certificate compressors");
    assert_eq!(compressors.len(), 1);
    let compressor = compressors[0];
    assert_eq!(compressor.algorithm(), CertificateCompressionAlgorithm::BROTLI);
    let binary: Vec<u8> = (0..=255).cycle().take(8192).collect();
    for input in [&[][..], &binary] {
        let mut compressed = Vec::new();
        apply(compressor.compress(), input, &mut compressed).expect("compress certificate");
        assert!(!compressed.is_empty());
        let mut restored = Vec::new();
        apply(compressor.decompress(), &compressed, &mut restored).expect("decompress certificate");
        assert_eq!(restored, input);
    }
}

struct RejectWrites;

impl Write for RejectWrites {
    fn write(&mut self, _input: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("certificate output rejected"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("certificate output rejected"))
    }
}

#[test]
fn certificate_compression_propagates_invalid_data_and_output_failures() {
    let profile = emulation();
    let tls = profile.tls_options.expect("TLS profile");
    let compressors = tls.certificate_compressors.expect("certificate compressors");
    let compressor = compressors[0];
    assert!(apply(compressor.decompress(), &[255; 32], &mut Vec::new()).is_err());
    assert!(apply(compressor.compress(), b"certificate", &mut RejectWrites).is_err());
    let mut compressed = Vec::new();
    apply(compressor.compress(), b"certificate", &mut compressed).expect("compress certificate");
    assert!(apply(compressor.decompress(), &compressed, &mut RejectWrites).is_err());
}

#[tokio::test]
async fn requests_send_the_chrome145_linux_headers() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("listen");
    let address = listener.local_addr().expect("address");
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let count = socket.read(&mut buffer).await.expect("read request");
            assert!(count > 0);
            request.extend_from_slice(&buffer[..count]);
            assert!(request.len() < 16384);
        }
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .expect("respond");
        String::from_utf8(request).expect("ASCII headers")
    });
    let client = wreq::Client::builder()
        .emulation(emulation())
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("client");
    let response = client.get(format!("http://{address}/")).send().await.expect("request");
    assert_eq!(response.text().await.expect("body"), "ok");
    let request = server.await.expect("server");
    let actual_headers = request
        .split("\r\n")
        .skip(1)
        .filter_map(|line| line.split_once(": "))
        .collect::<Vec<_>>();
    let profile = emulation();
    assert_eq!(profile.headers.len(), 10);
    for (name, value) in &profile.headers {
        assert!(
            actual_headers.contains(&(name.as_str(), value.to_str().expect("header value"))),
            "missing header {name}: {value:?} in {request}"
        );
    }
    assert!(!actual_headers.iter().any(|(name, _)| *name == "accept-encoding"));
}
