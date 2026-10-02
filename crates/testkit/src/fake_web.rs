//! A fake web for the web search: Wikipedia's API, Serper and plain pages,
//! all on one local server – and a record of every request, so tests can
//! prove what went out (and what did not).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

/// A request the fake web received.
#[derive(Debug, Clone)]
pub struct WebRequest {
    /// `wikipedia`, `serper` or `page`.
    pub kind: String,
    pub path: String,
    /// Query parameters (Wikipedia) or the JSON body (Serper).
    pub params: Value,
}

#[derive(Default)]
struct Inner {
    /// Language → title → (text, is a disambiguation page).
    articles: BTreeMap<String, BTreeMap<String, (String, bool)>>,
    serper_key: String,
    serper_answer: Value,
    pages: BTreeMap<String, String>,
    requests: Vec<WebRequest>,
}

#[derive(Clone)]
pub struct FakeWeb {
    addr: SocketAddr,
    inner: Arc<Mutex<Inner>>,
}

impl FakeWeb {
    pub async fn start() -> Self {
        let inner = Arc::new(Mutex::new(Inner {
            serper_key: "test-serper-key".into(),
            serper_answer: json!({"organic": []}),
            ..Default::default()
        }));
        let app = Router::new()
            .route("/wiki/{lang}/w/api.php", get(wikipedia))
            .route("/serper/search", post(serper))
            .route("/pages/{*path}", get(page))
            .with_state(inner.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { addr, inner }
    }

    /// For `wikipedia_endpoint` (`{lang}` stays a placeholder).
    pub fn wikipedia(&self) -> String {
        format!("http://{}/wiki/{{lang}}", self.addr)
    }

    /// For `serper_endpoint`.
    pub fn serper(&self) -> String {
        format!("http://{}/serper", self.addr)
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// A page address under a name the test maps to this server (`web_hosts`).
    pub fn page_url(&self, host: &str, path: &str) -> String {
        format!("http://{host}:{}/pages/{path}", self.addr.port())
    }

    pub fn article(&self, lang: &str, title: &str, text: &str) {
        self.inner
            .lock()
            .unwrap()
            .articles
            .entry(lang.into())
            .or_default()
            .insert(title.into(), (text.into(), false));
    }

    pub fn disambiguation(&self, lang: &str, title: &str) {
        self.inner
            .lock()
            .unwrap()
            .articles
            .entry(lang.into())
            .or_default()
            .insert(title.into(), (format!("{title} may mean:"), true));
    }

    pub fn serper_answer(&self, answer: Value) {
        self.inner.lock().unwrap().serper_answer = answer;
    }

    pub fn serper_key(&self) -> String {
        self.inner.lock().unwrap().serper_key.clone()
    }

    pub fn page(&self, path: &str, html: &str) {
        self.inner
            .lock()
            .unwrap()
            .pages
            .insert(path.into(), html.into());
    }

    pub fn requests(&self) -> Vec<WebRequest> {
        self.inner.lock().unwrap().requests.clone()
    }
}

type S = State<Arc<Mutex<Inner>>>;

async fn wikipedia(
    State(s): S,
    Path(lang): Path<String>,
    Query(q): Query<BTreeMap<String, String>>,
) -> Response {
    let mut inner = s.lock().unwrap();
    inner.requests.push(WebRequest {
        kind: "wikipedia".into(),
        path: lang.clone(),
        params: json!(q),
    });
    let articles = inner.articles.get(&lang).cloned().unwrap_or_default();
    let find = |t: &str| {
        articles
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(t))
            .map(|(k, v)| (k.clone(), v.clone()))
    };
    let body = if q.get("list").map(String::as_str) == Some("search") {
        let words: Vec<String> = q
            .get("srsearch")
            .cloned()
            .unwrap_or_default()
            .to_lowercase()
            .split_whitespace()
            .map(String::from)
            .collect();
        let limit: usize = q.get("srlimit").and_then(|l| l.parse().ok()).unwrap_or(3);
        let hits: Vec<Value> = articles
            .iter()
            .filter(|(t, (text, _))| {
                let hay = format!("{t} {text}").to_lowercase();
                words.iter().any(|w| hay.contains(w.as_str()))
            })
            .take(limit)
            .map(|(t, _)| json!({"title": t, "snippet": ""}))
            .collect();
        json!({"query": {"search": hits}})
    } else if q.get("prop").map(String::as_str) == Some("extracts") {
        let t = q.get("titles").cloned().unwrap_or_default();
        match find(&t) {
            Some((title, (text, _))) => {
                json!({"query": {"pages": [{"title": title, "extract": text}]}})
            }
            None => json!({"query": {"pages": [{"title": t, "missing": true}]}}),
        }
    } else {
        let t = q.get("titles").cloned().unwrap_or_default();
        match find(&t) {
            Some((title, (_, true))) => {
                json!({"query": {"pages": [{"title": title, "pageprops": {"disambiguation": ""}}]}})
            }
            Some((title, _)) => json!({"query": {"pages": [{"title": title}]}}),
            None => json!({"query": {"pages": [{"title": t, "missing": true}]}}),
        }
    };
    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn serper(State(s): S, headers: HeaderMap, body: String) -> Response {
    let mut inner = s.lock().unwrap();
    let params: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
    inner.requests.push(WebRequest {
        kind: "serper".into(),
        path: "search".into(),
        params,
    });
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if key != inner.serper_key {
        return (
            StatusCode::FORBIDDEN,
            json!({"message": "Unauthorized.", "statusCode": 403}).to_string(),
        )
            .into_response();
    }
    (
        [(header::CONTENT_TYPE, "application/json")],
        inner.serper_answer.to_string(),
    )
        .into_response()
}

async fn page(State(s): S, Path(path): Path<String>) -> Response {
    let mut inner = s.lock().unwrap();
    inner.requests.push(WebRequest {
        kind: "page".into(),
        path: path.clone(),
        params: Value::Null,
    });
    match inner.pages.get(&path) {
        Some(html) => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html.clone(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
