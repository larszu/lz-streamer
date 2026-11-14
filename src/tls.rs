//! HTTPS für die Weboberfläche: Zertifikat (selbst signiert oder eigenes),
//! TLS 1.2/1.3 über rustls und ein Listener, der auf demselben Port
//! TLS und Klartext unterscheidet.
//!
//! Klartext von 127.0.0.1/::1 wird weiter bedient (Grafik-Renderer, lokale
//! Werkzeuge), Klartext von außen bekommt eine Weiterleitung auf https://.
//! Die echte Gegenstelle bleibt als `ConnectInfo<SocketAddr>` erhalten.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, RwLock};
use std::task::{Context, Poll};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use pki_types::pem::PemObject;
use pki_types::{CertificateDer, PrivateKeyDer};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::{self, server::ClientHello, server::ResolvesServerCert, sign::CertifiedKey};

/// Gültigkeit des selbst signierten Zertifikats (Apple lässt höchstens 825 Tage zu)
const SELF_SIGNED_DAYS: i64 = 790;

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Angaben zum aktiven Zertifikat (ohne Schlüssel)
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct CertInfo {
    pub self_signed: bool,
    /// Namen und IP-Adressen im Zertifikat (nur bei selbst signiert bekannt)
    pub names: Vec<String>,
    /// gültig bis (JJJJ-MM-TT, nur bei selbst signiert bekannt)
    pub not_after: String,
    /// SHA-256 des Endzertifikats, hexadezimal mit Doppelpunkten
    pub sha256: String,
    /// Zertifikate in der Kette
    pub chain: usize,
}

fn fingerprint(der: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, der).as_ref().iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

/// Zertifikat und Schlüssel aus PEM prüfen und zusammensetzen.
pub fn certified_key(cert_pem: &[u8], key_pem: &[u8]) -> Result<(CertifiedKey, usize, String)> {
    let chain: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(cert_pem).collect::<Result<_, _>>().map_err(|e| anyhow!("Zertifikat nicht lesbar: {e:?}"))?;
    if chain.is_empty() {
        bail!("Kein Zertifikat (PEM „BEGIN CERTIFICATE“) gefunden");
    }
    let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| anyhow!("Schlüssel nicht lesbar: {e:?}"))?;
    let fp = fingerprint(&chain[0]);
    let n = chain.len();
    let ck = CertifiedKey::from_der(chain, key, &provider()).map_err(|e| anyhow!("Schlüssel passt nicht zum Zertifikat: {e}"))?;
    Ok((ck, n, fp))
}

/// Eigene Namen fürs selbst signierte Zertifikat: Rechnername, localhost, alle eigenen IPs.
pub fn own_names() -> Vec<String> {
    let mut v = Vec::new();
    if let Some(h) = sysinfo::System::host_name().filter(|h| !h.is_empty()) {
        v.push(h.clone());
        if !h.contains('.') {
            v.push(format!("{h}.local"));
        }
    }
    v.push("localhost".into());
    let nets = sysinfo::Networks::new_with_refreshed_list();
    let mut ips: Vec<IpAddr> = nets.values().flat_map(|d| d.ip_networks().iter().map(|n| n.addr)).collect();
    // IPv6 außer ::1 nicht: Privacy-Adressen wechseln laufend und erzwängen sonst ständig neue Zertifikate
    ips.retain(|ip| ip.is_ipv4());
    ips.extend([IpAddr::from([127, 0, 0, 1]), IpAddr::from(std::net::Ipv6Addr::LOCALHOST)]);
    ips.sort();
    ips.dedup();
    v.extend(ips.iter().map(|i| i.to_string()));
    v.dedup();
    v
}

/// Selbst signiertes Serverzertifikat: CN = erster Name, alle Namen als SAN.
pub fn self_signed(names: &[String]) -> Result<(String, String, String)> {
    use rcgen::{CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair};
    let mut p = CertificateParams::new(names.to_vec()).context("Namen fürs Zertifikat")?;
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, names.first().cloned().unwrap_or_else(|| "lz-streamer".into()));
    dn.push(DnType::OrganizationName, "lz-streamer");
    p.distinguished_name = dn;
    p.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let today = chrono::Utc::now().date_naive();
    let ymd = |d: chrono::NaiveDate| {
        use chrono::Datelike;
        rcgen::date_time_ymd(d.year(), d.month() as u8, d.day() as u8)
    };
    let until = today + chrono::Duration::days(SELF_SIGNED_DAYS);
    p.not_before = ymd(today - chrono::Duration::days(1));
    p.not_after = ymd(until);
    let key = KeyPair::generate()?;
    let cert = p.self_signed(&key)?;
    Ok((cert.pem(), key.serialize_pem(), until.format("%Y-%m-%d").to_string()))
}

/// Zertifikatsablage im Datenverzeichnis, austauschbar im laufenden Betrieb.
#[derive(Debug)]
pub struct CertStore {
    dir: PathBuf,
    current: RwLock<Option<Arc<CertifiedKey>>>,
    info: RwLock<CertInfo>,
}

impl ResolvesServerCert for CertStore {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        self.current.read().ok()?.clone()
    }
}

impl CertStore {
    pub fn new(dir: PathBuf) -> Arc<Self> {
        Arc::new(Self { dir, current: RwLock::new(None), info: RwLock::new(CertInfo::default()) })
    }
    fn cert_path(&self) -> PathBuf {
        self.dir.join("zertifikat.pem")
    }
    fn key_path(&self) -> PathBuf {
        self.dir.join("schluessel.pem")
    }
    fn info_path(&self) -> PathBuf {
        self.dir.join("zertifikat.json")
    }
    pub fn info(&self) -> CertInfo {
        self.info.read().map(|i| i.clone()).unwrap_or_default()
    }

    /// Vorhandenes Zertifikat laden. Fehlt es oder ist es selbst signiert und
    /// fehlt ein eigener Name bzw. läuft es binnen 30 Tagen ab: neu erzeugen.
    pub fn load_or_create(&self) -> Result<()> {
        let info: CertInfo = crate::admin::load_json(&self.info_path());
        let names = own_names();
        let expiring = chrono::NaiveDate::parse_from_str(&info.not_after, "%Y-%m-%d").is_ok_and(|d| d - chrono::Utc::now().date_naive() < chrono::Duration::days(30));
        // nur neu, wenn ein Name fehlt – sonst bliebe ein einmal vertrautes Zertifikat nicht stabil
        let stale = info.self_signed && (names.iter().any(|n| !info.names.contains(n)) || expiring);
        let files = self.cert_path().is_file() && self.key_path().is_file();
        if files && !stale {
            let (c, k) = (std::fs::read(self.cert_path())?, std::fs::read(self.key_path())?);
            match self.activate(&c, &k, info.clone()) {
                Ok(()) => return Ok(()),
                Err(e) => tracing::warn!("Zertifikat unbrauchbar, erzeuge ein neues: {e}"),
            }
        }
        self.create_self_signed(names)
    }

    pub fn create_self_signed(&self, names: Vec<String>) -> Result<()> {
        let (c, k, until) = self_signed(&names)?;
        let info = CertInfo { self_signed: true, names, not_after: until, ..Default::default() };
        self.store(c.as_bytes(), k.as_bytes(), info)
    }

    /// Eigenes Zertifikat (PEM, ggf. mit Zwischenzertifikaten) übernehmen.
    pub fn install(&self, cert_pem: &[u8], key_pem: &[u8]) -> Result<()> {
        self.store(cert_pem, key_pem, CertInfo::default())
    }

    fn store(&self, cert_pem: &[u8], key_pem: &[u8], info: CertInfo) -> Result<()> {
        certified_key(cert_pem, key_pem)?;
        std::fs::create_dir_all(&self.dir)?;
        write_private(&self.key_path(), key_pem)?;
        std::fs::write(self.cert_path(), cert_pem)?;
        self.activate(cert_pem, key_pem, info)
    }

    fn activate(&self, cert_pem: &[u8], key_pem: &[u8], mut info: CertInfo) -> Result<()> {
        let (ck, n, fp) = certified_key(cert_pem, key_pem)?;
        info.sha256 = fp;
        info.chain = n;
        crate::admin::save_json(&self.info_path(), &info)?;
        *self.current.write().map_err(|_| anyhow!("Sperre"))? = Some(Arc::new(ck));
        *self.info.write().map_err(|_| anyhow!("Sperre"))? = info;
        Ok(())
    }

    /// TLS-Konfiguration: TLS 1.3 und 1.2, HTTP/1.1
    pub fn server_config(self: &Arc<Self>) -> Result<Arc<rustls::ServerConfig>> {
        let mut c = rustls::ServerConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])?
            .with_no_client_auth()
            .with_cert_resolver(self.clone());
        c.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(c))
    }
}

/// Datei nur für den Eigentümer lesbar schreiben
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        use std::io::Write;
        o.open(&tmp)?.write_all(bytes)?;
    }
    std::fs::rename(tmp, path)
}

// ------------------------------------------------------------ Verbindungen

/// TLS oder Klartext (nur lokal)
pub enum Conn {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl AsyncRead for Conn {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Conn {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_flush(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Conn::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

/// Listener für `axum::serve`: Handshakes laufen nebenläufig, fertige
/// Verbindungen kommen über einen Kanal.
pub struct TlsListener {
    local: SocketAddr,
    rx: tokio::sync::mpsc::Receiver<(Conn, SocketAddr)>,
}

impl TlsListener {
    pub fn new(listener: TcpListener, config: Arc<rustls::ServerConfig>) -> io::Result<Self> {
        let local = listener.local_addr()?;
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let acceptor = TlsAcceptor::from(config);
        tokio::spawn(async move {
            loop {
                let (tcp, peer) = match listener.accept().await {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::debug!("accept: {e}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        continue;
                    }
                };
                let (tx, acceptor) = (tx.clone(), acceptor.clone());
                tokio::spawn(async move {
                    if let Some(c) = classify(tcp, peer, acceptor).await {
                        let _ = tx.send((c, peer)).await;
                    }
                });
            }
        });
        Ok(Self { local, rx })
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = Conn;
    type Addr = SocketAddr;
    async fn accept(&mut self) -> (Conn, SocketAddr) {
        match self.rx.recv().await {
            Some(x) => x,
            // der Annahme-Task endet nie; falls doch, nichts mehr annehmen
            None => std::future::pending().await,
        }
    }
    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }
}

/// Erstes Byte 0x16 = TLS-Handshake. Klartext von außen wird umgeleitet.
async fn classify(tcp: TcpStream, peer: SocketAddr, acceptor: TlsAcceptor) -> Option<Conn> {
    let mut first = [0u8; 1];
    let n = tokio::time::timeout(Duration::from_secs(10), tcp.peek(&mut first)).await.ok()?.ok()?;
    if n == 0 {
        return None;
    }
    if first[0] == 0x16 {
        return match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
            Ok(Ok(s)) => Some(Conn::Tls(Box::new(s))),
            Ok(Err(e)) => {
                tracing::debug!("TLS-Handshake mit {peer}: {e}");
                None
            }
            Err(_) => None,
        };
    }
    if peer.ip().is_loopback() || peer.ip().to_canonical().is_loopback() {
        return Some(Conn::Plain(tcp));
    }
    redirect(tcp).await;
    None
}

/// Kopf einer Klartext-Anfrage lesen und mit 308 auf https:// antworten.
async fn redirect(mut tcp: TcpStream) {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let read = async {
        while buf.len() < 8192 && !buf.windows(4).any(|w| w == b"\r\n\r\n") {
            match tcp.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(5), read).await;
    let local_port = tcp.local_addr().map(|a| a.port()).unwrap_or(443);
    let Some(location) = redirect_location(&String::from_utf8_lossy(&buf), local_port) else { return };
    let resp = format!("HTTP/1.1 308 Permanent Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = tcp.write_all(resp.as_bytes()).await;
    let _ = tcp.shutdown().await;
}

/// Ziel der Weiterleitung aus Anfragezeile und Host-Kopf
pub fn redirect_location(head: &str, port: u16) -> Option<String> {
    let mut lines = head.split("\r\n");
    let path = lines.next()?.split(' ').nth(1).filter(|p| p.starts_with('/'))?;
    let host = lines.find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("host").then(|| v.trim().to_string())
    })?;
    // Port aus dem Host-Kopf durch den echten ersetzen (gleicher Port für beides)
    let bare = if host.starts_with('[') { host.split_once(']').map(|(a, _)| format!("{a}]"))? } else { host.split(':').next()?.to_string() };
    if bare.is_empty() || bare.chars().any(|c| c.is_whitespace() || c == '/' || c == '@') {
        return None;
    }
    Some(if port == 443 { format!("https://{bare}{path}") } else { format!("https://{bare}:{port}{path}") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_roundtrip_and_mismatch() {
        let names = vec!["studio".to_string(), "localhost".into(), "127.0.0.1".into()];
        let (c, k, until) = self_signed(&names).unwrap();
        assert!(c.contains("BEGIN CERTIFICATE"));
        assert!(until.len() == 10);
        let (_, n, fp) = certified_key(c.as_bytes(), k.as_bytes()).unwrap();
        assert_eq!(n, 1);
        assert_eq!(fp.len(), 32 * 3 - 1);
        let (_, other_key, _) = self_signed(&names).unwrap();
        assert!(certified_key(c.as_bytes(), other_key.as_bytes()).is_err(), "fremder Schlüssel");
        assert!(certified_key(b"kein pem", k.as_bytes()).is_err());
    }

    #[test]
    fn store_regenerates_when_names_change() {
        let dir = std::env::temp_dir().join(format!("lzs-tls-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = CertStore::new(dir.clone());
        s.load_or_create().unwrap();
        let first = s.info();
        assert!(first.self_signed);
        assert!(first.names.contains(&"localhost".to_string()));
        // zweiter Start: gleiches Zertifikat
        let s2 = CertStore::new(dir.clone());
        s2.load_or_create().unwrap();
        assert_eq!(s2.info().sha256, first.sha256);
        // andere Namen gespeichert → neu erzeugen
        let mut i = first.clone();
        i.names = vec!["alt".into()];
        crate::admin::save_json(&dir.join("zertifikat.json"), &i).unwrap();
        let s3 = CertStore::new(dir.clone());
        s3.load_or_create().unwrap();
        assert_ne!(s3.info().sha256, first.sha256);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(dir.join("schluessel.pem")).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn redirect_target() {
        let h = "GET /api/state?x=1 HTTP/1.1\r\nHost: studio.local:4200\r\n\r\n";
        assert_eq!(redirect_location(h, 4200).as_deref(), Some("https://studio.local:4200/api/state?x=1"));
        assert_eq!(redirect_location("GET / HTTP/1.1\r\nhost: [fd00::5]:80\r\n\r\n", 443).as_deref(), Some("https://[fd00::5]/"));
        assert!(redirect_location("GET / HTTP/1.1\r\n\r\n", 4200).is_none(), "ohne Host");
        assert!(redirect_location("GET http://x/ HTTP/1.1\r\nHost: a\r\n\r\n", 4200).is_none());
        assert!(redirect_location("GET / HTTP/1.1\r\nHost: evil.com/x\r\n\r\n", 4200).is_none());
    }
}
