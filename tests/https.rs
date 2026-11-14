//! HTTPS: TLS 1.3/1.2 mit selbst signiertem und eigenem Zertifikat,
//! Klartext lokal, Weiterleitung von außen, echte Gegenstelle bleibt erhalten.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use lz_streamer::admin::DataDir;
use lz_streamer::caps::Capabilities;
use lz_streamer::config::Config;
use lz_streamer::ops::Ops;
use lz_streamer::server::{self, App};
use lz_streamer::supervisor::Supervisor;
use pki_types::pem::PemObject;
use pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::rustls;

async fn start(tag: &str) -> (SocketAddr, Arc<App>, std::path::PathBuf) {
    let _ = gstreamer::init();
    let root = std::env::temp_dir().join(format!("lzs-https-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let data = DataDir::new(root.clone());
    let sup = Supervisor::new(Config::default(), root.join("config.json"), root.join("run"), data.recordings());
    let app = App::new(sup, Ops::new(data), Capabilities::probe(), vec![]);
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(server::run(listener, app.clone(), true, std::future::pending()));
    // bis das Zertifikat liegt
    for _ in 0..100 {
        if app.https.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (addr, app, root)
}

fn client(cert_pem: &[u8], versions: &[&'static rustls::SupportedProtocolVersion]) -> tokio_rustls::TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    for c in CertificateDer::pem_slice_iter(cert_pem) {
        roots.add(c.unwrap()).unwrap();
    }
    let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(versions)
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(cfg))
}

async fn request<S: AsyncRead + AsyncWrite + Unpin>(s: &mut S, head: &str) -> String {
    s.write_all(head.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), s.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

fn get(path: &str) -> String {
    format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
}

const WS: &str = "GET /renderer HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";

/// Eine eigene, nicht-lokale Adresse dieses Rechners (für die Gegenstellen-Prüfung)
fn lan_ip() -> Option<IpAddr> {
    let nets = sysinfo::Networks::new_with_refreshed_list();
    nets.values().flat_map(|d| d.ip_networks().iter().map(|n| n.addr)).find(|ip| ip.is_ipv4() && !ip.is_loopback() && !ip.is_unspecified())
}

#[tokio::test]
async fn tls13_state_and_peer_address() {
    let (addr, _app, root) = start("tls13").await;
    let cert = std::fs::read(root.join("tls/zertifikat.pem")).unwrap();
    let local: SocketAddr = format!("127.0.0.1:{}", addr.port()).parse().unwrap();
    for (name, versions) in [("1.3", &[&rustls::version::TLS13][..]), ("1.2", &[&rustls::version::TLS12][..])] {
        let tcp = TcpStream::connect(local).await.unwrap();
        let mut tls = client(&cert, versions).connect(ServerName::try_from("localhost").unwrap(), tcp).await.unwrap();
        let want = if name == "1.3" { rustls::ProtocolVersion::TLSv1_3 } else { rustls::ProtocolVersion::TLSv1_2 };
        assert_eq!(tls.get_ref().1.protocol_version(), Some(want));
        let r = request(&mut tls, &get("/api/state")).await;
        assert!(r.starts_with("HTTP/1.1 200"), "TLS {name}: {r:.200}");
        assert!(r.contains("\"https\":true"), "TLS {name}");
    }
    // Zertifikat gilt auch für die IP
    let tcp = TcpStream::connect(local).await.unwrap();
    let mut tls = client(&cert, &[&rustls::version::TLS13]).connect(ServerName::try_from("127.0.0.1").unwrap(), tcp).await.unwrap();
    assert!(request(&mut tls, &get("/api/me")).await.starts_with("HTTP/1.1 200"));

    // Klartext lokal: weiter bedient; Renderer lokal per TLS: angenommen
    let mut plain = TcpStream::connect(local).await.unwrap();
    assert!(request(&mut plain, &get("/api/me")).await.starts_with("HTTP/1.1 200"));
    let tcp = TcpStream::connect(local).await.unwrap();
    let mut tls = client(&cert, &[&rustls::version::TLS13]).connect(ServerName::try_from("localhost").unwrap(), tcp).await.unwrap();
    tls.write_all(WS.as_bytes()).await.unwrap();
    let mut buf = [0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(5), tls.read(&mut buf)).await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 101"), "Renderer lokal");

    // Von einer Netzadresse: Renderer abgelehnt (Gegenstelle ist nicht 127.0.0.1), Klartext umgeleitet
    let Some(ip) = lan_ip() else {
        eprintln!("keine Netzadresse – Gegenstellen-Prüfung übersprungen");
        return;
    };
    let lan = SocketAddr::new(ip, addr.port());
    let tcp = TcpStream::connect(lan).await.unwrap();
    let mut tls = client(&cert, &[&rustls::version::TLS13]).connect(ServerName::try_from("localhost").unwrap(), tcp).await.unwrap();
    let r = request(&mut tls, WS).await;
    assert!(r.starts_with("HTTP/1.1 403"), "Renderer von außen: {r:.200}");
    let mut plain = TcpStream::connect(lan).await.unwrap();
    let r = request(&mut plain, &format!("GET /api/state HTTP/1.1\r\nHost: {ip}:80\r\n\r\n")).await;
    assert!(r.starts_with("HTTP/1.1 308"), "{r}");
    assert!(r.contains(&format!("Location: https://{ip}:{}/api/state", addr.port())), "{r}");
}

#[tokio::test]
async fn own_certificate_upload_applies_without_restart() {
    let (addr, app, root) = start("own").await;
    let local: SocketAddr = format!("127.0.0.1:{}", addr.port()).parse().unwrap();
    let before = app.certs.info();
    assert!(before.self_signed);
    let (cert, key, _) = lz_streamer::tls::self_signed(&["studio.example".into(), "127.0.0.1".into()]).unwrap();
    // Upload über die API (Klartext lokal)
    let body = serde_json::json!({"cert": cert, "key": key}).to_string();
    let mut plain = TcpStream::connect(local).await.unwrap();
    let r = request(&mut plain, &format!("POST /api/tls HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())).await;
    assert!(r.starts_with("HTTP/1.1 200"), "{r}");
    assert!(!r.contains("PRIVATE KEY"), "Schlüssel kommt nie zurück");
    let info = app.certs.info();
    assert!(!info.self_signed);
    assert_ne!(info.sha256, before.sha256);
    // neue Verbindung sieht das neue Zertifikat
    let tcp = TcpStream::connect(local).await.unwrap();
    let mut tls = client(cert.as_bytes(), &[&rustls::version::TLS13]).connect(ServerName::try_from("studio.example").unwrap(), tcp).await.unwrap();
    assert!(request(&mut tls, &get("/api/tls")).await.contains("\"self_signed\":false"));
    // falscher Schlüssel wird abgelehnt, altes Zertifikat bleibt
    let (_, other, _) = lz_streamer::tls::self_signed(&["x".into()]).unwrap();
    assert!(app.certs.install(cert.as_bytes(), other.as_bytes()).is_err());
    assert_eq!(app.certs.info().sha256, info.sha256);
    // eigenes Zertifikat übersteht den Neustart
    let again = lz_streamer::tls::CertStore::new(root.join("tls"));
    again.load_or_create().unwrap();
    assert_eq!(again.info().sha256, info.sha256);
}
