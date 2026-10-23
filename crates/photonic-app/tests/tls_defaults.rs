//! Exercise the application's default HTTP client without contacting a provider.
//! Linux native TLS honors SSL_CERT_FILE; WebPKI must remain independent of it.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Fixture {
    server: Child,
    dir: std::path::PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.server.kill();
        let _ = self.server.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn default_client_rejects_native_only_ca() {
    // A child isolates SSL_CERT_FILE from parallel tests and the host trust store.
    if let Ok(url) = std::env::var("PHOTONIC_TLS_TEST_URL") {
        let err = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
            .get(url)
            .send()
            .expect_err("default client must reject a CA trusted only by native TLS");
        assert!(err.is_connect(), "expected certificate rejection: {err:?}");
        assert!(format!("{err:?}").contains("UnknownIssuer"), "{err:?}");
        return;
    }

    let dir = std::env::temp_dir().join(format!("photonic-tls-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    let cert = dir.join("cert.pem");
    let key = dir.join("key.pem");
    let generated = Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
        ])
        .args([
            "-subj",
            "/CN=localhost",
            "-addext",
            "subjectAltName=DNS:localhost",
            "-addext",
            "basicConstraints=critical,CA:FALSE",
        ])
        .arg("-keyout")
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .output()
        .expect("openssl fixture generator");
    assert!(generated.status.success(), "fixture generation failed");

    let server = Command::new("python3")
        .args([
            "-u",
            "-c",
            r#"
import socket, ssl, sys
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(sys.argv[1], sys.argv[2])
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    listener.listen()
    listener.settimeout(15)
    print(listener.getsockname()[1], flush=True)
    while True:
        connection, _ = listener.accept()
        connection.settimeout(5)
        try:
            with ctx.wrap_socket(connection, server_side=True) as tls:
                tls.recv(16384)
                tls.sendall(b'HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok')
        except (ssl.SSLError, OSError):
            connection.close()
"#,
        ])
        .arg(&cert)
        .arg(&key)
        .stdout(Stdio::piped())
        .spawn()
        .expect("loopback TLS fixture");
    let mut fixture = Fixture { server, dir };
    let mut port = String::new();
    BufReader::new(fixture.server.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let port: u16 = port.trim().parse().expect("fixture port");
    let url = format!("https://localhost:{port}/");

    // Positive control: an explicitly configured fixture CA works normally.
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .add_root_certificate(
            reqwest::Certificate::from_pem(&std::fs::read(&cert).unwrap()).unwrap(),
        )
        .build()
        .unwrap();
    assert_eq!(client.get(&url).send().unwrap().text().unwrap(), "ok");
    // The same trusted certificate cannot authenticate a different hostname.
    assert!(client
        .get(format!("https://127.0.0.1:{port}/"))
        .send()
        .is_err());

    let probe = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "default_client_rejects_native_only_ca",
            "--nocapture",
        ])
        .env("PHOTONIC_TLS_TEST_URL", &url)
        .env("SSL_CERT_FILE", &cert)
        .env("SSL_CERT_DIR", &fixture.dir)
        .output()
        .unwrap();
    assert!(
        probe.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&probe.stdout),
        String::from_utf8_lossy(&probe.stderr),
    );
}
