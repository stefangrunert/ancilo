//! The one door to the network. Every request of Ancilo's that can leave
//! this computer is sent through a [`Net`] – and written into the user's log
//! of what left it (System › What left this Mac): when, why, to whom, and
//! what was sent. Requests to this computer itself (the local model, the
//! daemon) are not logged: nothing leaves.
//!
//! What is never written: headers – so no key, token or password, which
//! travel only there. What is: the address (with its query – a search term
//! is what the user wants to see) and the body as sent (a search, a message
//! to a cloud model), clipped.
//!
//! `xtask` checks that no other code sends: a request outside this crate is
//! a test failure (except the allowlisted loopback-only clients).

use std::sync::Arc;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why something left – the app says it in the user's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// A web search (Wikipedia, Google through Serper).
    WebSearch,
    /// A page a web search found, read for its text.
    WebPage,
    /// Looking for models on Hugging Face.
    ModelSearch,
    /// A model's files and facts on Hugging Face (before downloading).
    ModelInfo,
    /// A model's description (its model card).
    ModelCard,
    /// Downloading a model.
    ModelDownload,
    /// The list of recommended models (ancilo's catalog).
    Catalog,
    /// Downloading llama.cpp, the program that runs models.
    LlamaDownload,
    /// A message to a model in the cloud.
    CloudModel,
    /// Checking a cloud model's address while setting it up.
    CloudSetup,
    /// Looking for a new version of Ancilo.
    UpdateCheck,
    /// Downloading a new version of Ancilo.
    UpdateDownload,
}

/// Who started it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum By {
    /// The user (a click, a question, a command).
    You,
    /// Ancilo by itself (a download resumed, a description fetched, a daily update check).
    Ancilo,
}

/// What a request is for – given where it is sent.
#[derive(Debug, Clone)]
pub struct Note {
    pub purpose: Purpose,
    /// What it is about, in the user's terms: the search term, the model, the file.
    pub subject: String,
    pub by: By,
}

impl Note {
    pub fn new(purpose: Purpose, subject: impl Into<String>, by: By) -> Self {
        Self {
            purpose,
            subject: subject.into(),
            by,
        }
    }
}

/// One request that left this computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Departure {
    pub at: DateTime<Utc>,
    pub purpose: Purpose,
    pub subject: String,
    pub by: By,
    pub method: String,
    /// Where it went: the host alone (simple view) …
    pub host: String,
    /// … and the whole address (details).
    pub url: String,
    /// The body as sent, as text – clipped; `None`: nothing but the address.
    pub sent: Option<String>,
    pub sent_bytes: u64,
    /// What came back (from the answer's length, when it says).
    pub received_bytes: Option<u64>,
    /// Where the answer came from, when it was forwarded elsewhere (a
    /// download's file server).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirected_to: Option<String>,
    /// The answer's status, or why there was none.
    pub status: Option<u16>,
    pub error: Option<String>,
}

/// Keeps the log (the daemon: its database).
pub trait Recorder: Send + Sync {
    fn record(&self, d: Departure);
}

impl std::fmt::Debug for dyn Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Recorder")
    }
}

/// A body is kept up to this many bytes (a cloud conversation can be long).
pub const MAX_SENT: usize = 64 * 1024;

/// A client whose requests are logged when they leave this computer.
#[derive(Clone)]
pub struct Net {
    client: reqwest::Client,
    log: Option<Arc<dyn Recorder>>,
}

impl std::fmt::Debug for Net {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Net")
            .field("logged", &self.log.is_some())
            .finish()
    }
}

impl Net {
    /// `client` as built by its owner (timeouts, resolver, redirects);
    /// `log`: where departures go (`None`: tests and tools without a daemon).
    pub fn new(client: reqwest::Client, log: Option<Arc<dyn Recorder>>) -> Self {
        Self { client, log }
    }

    pub fn get(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.client.get(url)
    }

    pub fn post(&self, url: impl reqwest::IntoUrl) -> reqwest::RequestBuilder {
        self.client.post(url)
    }

    /// Sends the request – logged if it leaves this computer.
    pub async fn send(
        &self,
        request: reqwest::RequestBuilder,
        note: Note,
    ) -> reqwest::Result<reqwest::Response> {
        let request = request.build()?;
        let url = request.url().clone();
        let leaves = !is_local(&url);
        let (method, body) = (
            request.method().to_string(),
            request
                .body()
                .and_then(|b| b.as_bytes())
                .map(<[u8]>::to_vec),
        );
        let result = self.client.execute(request).await;
        if leaves && let Some(log) = &self.log {
            let (status, received_bytes, redirected_to, error) = match &result {
                Ok(r) => (
                    Some(r.status().as_u16()),
                    r.content_length(),
                    (r.url() != &url).then(|| r.url().to_string()),
                    None,
                ),
                Err(e) => (
                    e.status().map(|s| s.as_u16()),
                    None,
                    None,
                    Some(e.to_string()),
                ),
            };
            log.record(Departure {
                at: Utc::now(),
                purpose: note.purpose,
                subject: note.subject,
                by: note.by,
                method,
                host: url.host_str().unwrap_or_default().to_string(),
                url: url.to_string(),
                sent_bytes: body.as_ref().map_or(0, |b| b.len() as u64),
                sent: body.map(|b| clip(&String::from_utf8_lossy(&b))),
                received_bytes,
                redirected_to,
                status,
                error,
            });
        }
        result
    }
}

/// A client builder that reaches these host names at these addresses
/// (tests: fakes that look like servers out there – `Config.web_hosts`).
pub fn with_hosts<'a>(
    mut builder: reqwest::ClientBuilder,
    hosts: impl IntoIterator<Item = (&'a String, &'a std::net::IpAddr)>,
) -> reqwest::ClientBuilder {
    for (name, ip) in hosts {
        builder = builder.resolve(name, std::net::SocketAddr::new(*ip, 0));
    }
    builder
}

/// This computer itself: the local model, the daemon.
pub fn is_local(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(d)) => d == "localhost" || d.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => true,
    }
}

fn clip(text: &str) -> String {
    if text.len() <= MAX_SENT {
        return text.to_string();
    }
    let mut end = MAX_SENT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} … ({} bytes in all)", &text[..end], text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Log(Mutex<Vec<Departure>>);
    impl Recorder for Log {
        fn record(&self, d: Departure) {
            self.0.lock().unwrap().push(d);
        }
    }

    #[test]
    fn this_computer_is_local() {
        for (u, local) in [
            ("http://127.0.0.1:7424/v1", true),
            ("http://localhost:8080", true),
            ("http://[::1]:1/", true),
            ("https://de.wikipedia.org/w/api.php", false),
            ("http://192.168.1.20:11434/v1", false),
        ] {
            assert_eq!(is_local(&reqwest::Url::parse(u).unwrap()), local, "{u}");
        }
    }

    // covers: M11-AC-01
    /// What leaves is logged – address, body and answer, never a header
    /// (keys travel there); what stays here is not.
    #[tokio::test]
    async fn what_leaves_is_logged_without_its_keys() {
        let app = axum::Router::new().route(
            "/search",
            axum::routing::post(|| async { "{\"organic\": []}" }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.ok() });
        let log = Arc::new(Log::default());
        // A name that resolves to this computer, but is not "local" by its
        // address: as if it were a server out there.
        let client = reqwest::Client::builder()
            .resolve("search.example", ([127, 0, 0, 1], port).into())
            .build()
            .unwrap();
        let net = Net::new(client, Some(log.clone()));
        let url = format!("http://search.example:{port}/search");
        net.send(
            net.post(&url)
                .header("X-API-KEY", "secret-key")
                .body("{\"q\":\"Gaustatoppen\"}"),
            Note::new(Purpose::WebSearch, "Gaustatoppen", By::You),
        )
        .await
        .unwrap();
        // The same server by its loopback address: nothing left.
        net.send(
            net.post(format!("http://127.0.0.1:{port}/search")),
            Note::new(Purpose::CloudModel, "x", By::You),
        )
        .await
        .unwrap();
        let got = log.0.lock().unwrap().clone();
        assert_eq!(got.len(), 1, "{got:?}");
        let d = &got[0];
        assert_eq!(
            (d.purpose, d.by, d.status),
            (Purpose::WebSearch, By::You, Some(200))
        );
        assert_eq!(d.host, "search.example");
        assert_eq!(d.sent.as_deref(), Some("{\"q\":\"Gaustatoppen\"}"));
        assert_eq!(d.received_bytes, Some(15));
        assert!(!serde_json::to_string(d).unwrap().contains("secret-key"));
    }

    #[test]
    fn long_bodies_are_clipped_on_a_character() {
        let long = "ä".repeat(MAX_SENT);
        let c = clip(&long);
        assert!(c.len() < long.len() && c.ends_with("bytes in all)"));
    }
}
