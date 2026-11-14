//! Alarme per Mail (SMTP). Zugangsdaten stehen nur in `verwaltung.json`;
//! das Passwort verlässt den Rechner nur in Richtung Mailserver.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Security {
    /// Port 587, Verschlüsselung nach dem Verbindungsaufbau
    #[default]
    Starttls,
    /// Port 465, von Anfang an verschlüsselt
    Tls,
    /// unverschlüsselt (nur im eigenen Netz)
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MailSettings {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub user: String,
    /// wird über die API nie zurückgegeben
    pub password: String,
    /// Absender; leer = Benutzername (wenn eine Adresse) oder lz-streamer@localhost
    pub from: String,
    /// Empfänger, mehrere durch Komma getrennt
    pub to: String,
    /// nur Fehler melden, keine Warnungen
    pub errors_only: bool,
    /// Entwarnung schicken, wenn ein Alarm erledigt ist
    pub cleared: bool,
}

impl Default for MailSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            host: String::new(),
            port: 587,
            security: Security::Starttls,
            user: String::new(),
            password: String::new(),
            from: String::new(),
            to: String::new(),
            errors_only: false,
            cleared: true,
        }
    }
}

impl MailSettings {
    /// Was noch fehlt, damit Mails rausgehen (Hinweis, keine Sperre).
    pub fn hint(&self) -> Option<&'static str> {
        if self.host.trim().is_empty() {
            Some("Kein Mailserver eingetragen")
        } else if self.recipients().is_empty() {
            Some("Kein Empfänger eingetragen")
        } else {
            None
        }
    }

    pub fn recipients(&self) -> Vec<&str> {
        self.to.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()).collect()
    }

    fn sender(&self) -> String {
        let f = self.from.trim();
        if !f.is_empty() {
            f.to_string()
        } else if self.user.contains('@') {
            self.user.trim().to_string()
        } else {
            "lz-streamer@localhost".to_string()
        }
    }
}

/// Eine Mail an alle Empfänger schicken.
pub async fn send(s: &MailSettings, subject: &str, body: &str) -> Result<()> {
    if let Some(h) = s.hint() {
        bail!("{h}");
    }
    let from: Mailbox = s.sender().parse().with_context(|| format!("Absender „{}“ ist keine Mailadresse", s.sender()))?;
    let mut b = Message::builder().from(from).subject(subject).header(ContentType::TEXT_PLAIN);
    for r in s.recipients() {
        b = b.to(r.parse().with_context(|| format!("Empfänger „{r}“ ist keine Mailadresse"))?);
    }
    let msg = b.body(body.to_string())?;
    let host = s.host.trim();
    let mut t = match s.security {
        Security::Starttls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)?,
        Security::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(host)?,
        Security::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host),
    }
    .port(s.port)
    .timeout(Some(Duration::from_secs(15)));
    if !s.user.trim().is_empty() {
        t = t.credentials(Credentials::new(s.user.trim().to_string(), s.password.clone()));
    }
    t.build().send(msg).await.context("Mailserver")?;
    Ok(())
}

/// Betreff und Text einer Alarm-Mail. `fired` und `cleared` sind (Stufe, Text).
pub fn alarm_mail(host: &str, fired: &[(&str, String)], cleared: &[String]) -> (String, String) {
    let subject = match (fired, cleared) {
        ([(_, t)], []) => format!("[lz-streamer {host}] Alarm: {t}"),
        ([], [t]) => format!("[lz-streamer {host}] Erledigt: {t}"),
        ([], _) => format!("[lz-streamer {host}] {} Alarme erledigt", cleared.len()),
        _ => format!("[lz-streamer {host}] {} Alarme", fired.len()),
    };
    let mut body = format!("lz-streamer auf {host}, {}\n\n", chrono::Local::now().format("%d.%m.%Y %H:%M:%S"));
    for (level, t) in fired {
        body.push_str(&format!("{}: {t}\n", if *level == "error" { "FEHLER" } else { "Warnung" }));
    }
    for t in cleared {
        body.push_str(&format!("Erledigt: {t}\n"));
    }
    (subject, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_and_sender() {
        let mut s = MailSettings::default();
        assert_eq!(s.hint(), Some("Kein Mailserver eingetragen"));
        s.host = "mail.example.org".into();
        assert_eq!(s.hint(), Some("Kein Empfänger eingetragen"));
        s.to = "a@example.org; b@example.org,".into();
        assert_eq!(s.recipients(), vec!["a@example.org", "b@example.org"]);
        assert_eq!(s.hint(), None);
        assert_eq!(s.sender(), "lz-streamer@localhost");
        s.user = "technik@example.org".into();
        assert_eq!(s.sender(), "technik@example.org");
    }

    #[test]
    fn alarm_subjects() {
        let (s, b) = alarm_mail("regie", &[("error", "Kein Eingangssignal".into())], &[]);
        assert_eq!(s, "[lz-streamer regie] Alarm: Kein Eingangssignal");
        assert!(b.contains("FEHLER: Kein Eingangssignal"));
        let (s, _) = alarm_mail("regie", &[], &["CPU 95 %".into()]);
        assert!(s.contains("Erledigt: CPU 95 %"));
    }
}
