//! Exercise the application's default HTTP client without contacting a provider.
//! OS/custom roots must work through patched WebPKI, including behind a proxy.
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
fn default_clients_preserve_custom_trust_and_proxy() {
    // A child isolates SSL_CERT_FILE from parallel tests and the host trust store.
    if let Ok(url) = std::env::var("PHOTONIC_TLS_TEST_URL") {
        let trusted = std::env::var("PHOTONIC_TLS_TEST_TRUSTED").unwrap() == "1";
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client.get(&url).send();
        let api = hf_hub::api::sync::ApiBuilder::new()
            .with_endpoint(url.trim_end_matches('/').to_owned())
            .with_token(None)
            .with_progress(false)
            .build()
            .unwrap();
        let info = api.model("owned-fixture".into()).info();
        if trusted {
            assert_eq!(
                response.unwrap().text().unwrap(),
                r#"{"siblings":[],"sha":"owned-fixture"}"#
            );
            assert_eq!(info.unwrap().sha, "owned-fixture");
            let wrong_host = url.replace("localhost", "127.0.0.1");
            let err = client.get(&wrong_host).send().unwrap_err();
            assert!(format!("{err:?}").contains("NotValidForName"), "{err:?}");
            let wrong_api = hf_hub::api::sync::ApiBuilder::new()
                .with_endpoint(wrong_host.trim_end_matches('/').to_owned())
                .with_token(None)
                .with_progress(false)
                .build()
                .unwrap();
            assert!(wrong_api.model("owned-fixture".into()).info().is_err());
        } else {
            let err = response.unwrap_err();
            assert!(format!("{err:?}").contains("UnknownIssuer"), "{err:?}");
            let err = info.unwrap_err();
            assert!(
                format!("{err:?}").to_lowercase().contains("certificate"),
                "{err:?}"
            );
        }
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
import socket, ssl, sys, threading, select
ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
ctx.load_cert_chain(sys.argv[1], sys.argv[2])
with socket.socket() as listener:
    listener.bind(('127.0.0.1', 0))
    listener.listen()
    listener.settimeout(15)
    tls_port = listener.getsockname()[1]
    proxy = socket.socket()
    proxy.bind(('127.0.0.1', 0))
    proxy.listen()
    def tunnel(connection):
        with connection:
            request = b''
            while not request.endswith(b'\r\n\r\n'):
                block = connection.recv(1)
                if not block or len(request) > 16384:
                    return
                request += block
            # This proxy can only reach our owned TLS fixture.
            target = request.split(b' ')[1].decode()
            if target not in [f'localhost:{tls_port}', f'127.0.0.1:{tls_port}']:
                return
            with socket.create_connection(('127.0.0.1', tls_port), timeout=5) as upstream:
                connection.sendall(b'HTTP/1.1 200 Connection established\r\n\r\n')
                while True:
                    readable, _, _ = select.select([connection, upstream], [], [], 10)
                    if not readable:
                        return
                    for source in readable:
                        data = source.recv(16384)
                        if not data:
                            return
                        (upstream if source is connection else connection).sendall(data)
    def serve_proxy():
        while True:
            connection, _ = proxy.accept()
            threading.Thread(target=tunnel, args=(connection,), daemon=True).start()
    threading.Thread(target=serve_proxy, daemon=True).start()
    print(tls_port, proxy.getsockname()[1], flush=True)
    while True:
        connection, _ = listener.accept()
        connection.settimeout(5)
        try:
            with ctx.wrap_socket(connection, server_side=True) as tls:
                tls.recv(16384)
                body = b'{"siblings":[],"sha":"owned-fixture"}'
                tls.sendall(b'HTTP/1.1 200 OK\r\nContent-Length: ' + str(len(body)).encode() + b'\r\nConnection: close\r\n\r\n' + body)
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
    let mut ports = port.split_whitespace();
    let port: u16 = ports.next().unwrap().parse().expect("fixture port");
    let proxy_port: u16 = ports.next().unwrap().parse().expect("proxy port");
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
    assert_eq!(
        client.get(&url).send().unwrap().text().unwrap(),
        r#"{"siblings":[],"sha":"owned-fixture"}"#
    );
    // The same trusted certificate cannot authenticate a different hostname.
    assert!(client
        .get(format!("https://127.0.0.1:{port}/"))
        .send()
        .is_err());

    let empty_store = fixture.dir.join("empty-store");
    std::fs::create_dir(&empty_store).unwrap();
    std::fs::write(empty_store.join("empty.pem"), b"").unwrap();
    for (trusted, proxy) in [(false, false), (true, false), (true, true)] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "default_clients_preserve_custom_trust_and_proxy",
                "--nocapture",
            ])
            .env("PHOTONIC_TLS_TEST_URL", &url)
            .env("PHOTONIC_TLS_TEST_TRUSTED", if trusted { "1" } else { "0" })
            .env(
                "SSL_CERT_FILE",
                if trusted {
                    cert.clone()
                } else {
                    empty_store.join("empty.pem")
                },
            )
            .env("SSL_CERT_DIR", &empty_store);
        // No inherited endpoint/proxy/token settings can escape this loopback fixture.
        for name in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
            "HF_TOKEN",
        ] {
            command.env_remove(name);
        }
        if proxy {
            command.env("HTTPS_PROXY", format!("http://127.0.0.1:{proxy_port}"));
        }
        let probe = command.output().unwrap();
        assert!(
            probe.status.success(),
            "trusted={trusted} proxy={proxy}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&probe.stdout),
            String::from_utf8_lossy(&probe.stderr)
        );
    }
}
