use rustls::client::danger::ServerCertVerifier;
use rustls::pki_types::{pem::PemObject, CertificateDer, ServerName, UnixTime};
use std::{sync::Arc, time::SystemTime};
#[test]
fn default_webpki_rejects_public_compromised_chains() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/public-chains");
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        Arc::new(rustls::crypto::ring::default_provider()),
    )
    .build()
    .unwrap();
    let mut count = 0;
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("pem") {
            continue;
        }
        let pem = std::fs::read(&path).unwrap();
        let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&pem)
            .collect::<Result<_, _>>()
            .unwrap();
        let name = path.file_stem().unwrap().to_str().unwrap().to_string();
        let result = verifier.verify_server_cert(
            &chain[0],
            &chain[1..],
            &ServerName::try_from(name.clone()).unwrap(),
            &[],
            UnixTime::since_unix_epoch(
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap(),
            ),
        );
        println!("{name}: {result:?}");
        assert!(
            matches!(
                result,
                Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::Other(_)
                ))
            ),
            "expected explicit distrust: {result:?}"
        );
        assert!(format!("{result:?}").contains("CertDistrusted"));
        count += 1;
    }
    assert_eq!(count, 14);
}
