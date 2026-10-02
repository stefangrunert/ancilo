//! The search providers: Wikipedia (the MediaWiki API, no account) and Serper
//! (Google results, the user's key). Each turns a planned lookup into pages.

use ancilo_core::{Error, Result};
use reqwest::{Client, StatusCode, Url};
use serde_json::{Value, json};

/// Answers from providers are read up to this size.
pub const MAX_ANSWER_BYTES: usize = 1 << 20;

/// A page of text for the passage choice.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub title: String,
    pub url: String,
    pub text: String,
}

/// What to look up (from the planning step).
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// A short search query.
    pub query: String,
    /// The subject as an article title, if known.
    pub topic: Option<String>,
    /// Two-letter language code.
    pub lang: String,
}

/// A language code that may go into a host name (`de`, `en`, `nds`, …).
pub fn language(code: &str) -> String {
    let c = code.trim().to_ascii_lowercase();
    if (2..=3).contains(&c.len()) && c.chars().all(|ch| ch.is_ascii_lowercase()) {
        c
    } else {
        "en".into()
    }
}

async fn read_json(res: reqwest::Response) -> Result<Value> {
    let mut res = res;
    let mut bytes = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| {
        Error::unavailable(format!("the search service did not answer completely: {e}"))
    })? {
        if bytes.len() + chunk.len() > MAX_ANSWER_BYTES {
            return Err(Error::unavailable(
                "the search service answered with too much data",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::unavailable(format!("the search service answered strangely: {e}")))
}

fn offline(e: reqwest::Error) -> Error {
    Error::unavailable(format!(
        "the search service is not reachable – is this computer online? ({e})"
    ))
}

/// Wikipedia through its official API. `base`: `https://{lang}.wikipedia.org`.
pub struct Wikipedia<'a> {
    pub client: &'a Client,
    pub base: &'a str,
}

impl Wikipedia<'_> {
    fn api(&self, lang: &str) -> Result<Url> {
        Url::parse(&format!("{}/w/api.php", self.base.replace("{lang}", lang)))
            .map_err(Error::internal)
    }

    async fn get(&self, lang: &str, params: &[(&str, &str)]) -> Result<Value> {
        let mut url = self.api(lang)?;
        url.query_pairs_mut()
            .extend_pairs(params)
            .append_pair("format", "json")
            .append_pair("formatversion", "2");
        let res = self.client.get(url).send().await.map_err(offline)?;
        if !res.status().is_success() {
            return Err(Error::unavailable(format!(
                "Wikipedia answered HTTP {}",
                res.status().as_u16()
            )));
        }
        read_json(res).await
    }

    fn page_url(&self, lang: &str, title: &str) -> String {
        let mut url = match Url::parse(&self.base.replace("{lang}", lang)) {
            Ok(u) => u,
            Err(_) => return String::new(),
        };
        if let Ok(mut segs) = url.path_segments_mut() {
            segs.pop_if_empty()
                .push("wiki")
                .push(&title.replace(' ', "_"));
        }
        url.to_string()
    }

    /// The articles that fit: the one titled like the topic (not a
    /// disambiguation page), then the best search hits – at most two, with text.
    pub async fn pages(&self, q: &Query) -> Result<Vec<Page>> {
        let lang = language(&q.lang);
        let mut titles: Vec<String> = Vec::new();
        if let Some(topic) = q.topic.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            let exact = self
                .get(
                    &lang,
                    &[
                        ("action", "query"),
                        ("titles", topic),
                        ("redirects", "1"),
                        ("prop", "pageprops"),
                        ("ppprop", "disambiguation"),
                    ],
                )
                .await?;
            let page = &exact["query"]["pages"][0];
            if page["missing"].as_bool() != Some(true)
                && page["pageprops"]["disambiguation"].is_null()
                && page["title"].is_string()
            {
                titles.push(page["title"].as_str().unwrap_or_default().to_string());
            } else {
                let found = self
                    .get(
                        &lang,
                        &[
                            ("action", "query"),
                            ("list", "search"),
                            ("srsearch", topic),
                            ("srlimit", "1"),
                        ],
                    )
                    .await?;
                titles.extend(
                    found["query"]["search"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|h| h["title"].as_str().map(String::from)),
                );
            }
        }
        let found = self
            .get(
                &lang,
                &[
                    ("action", "query"),
                    ("list", "search"),
                    ("srsearch", &q.query),
                    ("srlimit", "3"),
                ],
            )
            .await?;
        for hit in found["query"]["search"].as_array().into_iter().flatten() {
            if let Some(t) = hit["title"].as_str()
                && !titles.iter().any(|x| x == t)
            {
                titles.push(t.to_string());
            }
        }
        let mut pages = Vec::new();
        for title in titles.into_iter().take(2) {
            let v = self
                .get(
                    &lang,
                    &[
                        ("action", "query"),
                        ("prop", "extracts"),
                        ("explaintext", "1"),
                        ("titles", &title),
                        ("redirects", "1"),
                    ],
                )
                .await?;
            let page = &v["query"]["pages"][0];
            let text: String = page["extract"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(200_000)
                .collect();
            if text.trim().is_empty() {
                continue;
            }
            let title = page["title"].as_str().unwrap_or(&title).to_string();
            pages.push(Page {
                url: self.page_url(&lang, &title),
                title,
                text,
            });
        }
        Ok(pages)
    }
}

/// A Serper answer: boxes Google shows above the results (often the answer
/// itself) and the result list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SerperAnswer {
    pub boxes: Vec<Page>,
    /// Title, link, snippet.
    pub organic: Vec<Page>,
}

/// Google results through Serper (`POST {base}/search`, key in `X-API-KEY`).
pub struct Serper<'a> {
    pub client: &'a Client,
    pub base: &'a str,
    pub key: &'a str,
}

impl Serper<'_> {
    pub async fn search(&self, q: &Query) -> Result<SerperAnswer> {
        let lang = language(&q.lang);
        let res = self
            .client
            .post(format!("{}/search", self.base.trim_end_matches('/')))
            .header("X-API-KEY", self.key)
            .json(&json!({"q": q.query, "hl": lang, "num": 8}))
            .send()
            .await
            .map_err(offline)?;
        match res.status() {
            s if s.is_success() => {}
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                return Err(Error::Unauthorized(
                    "Serper does not accept this key – copy it again from serper.dev (API key)"
                        .into(),
                ));
            }
            StatusCode::TOO_MANY_REQUESTS => {
                return Err(Error::unavailable(
                    "Serper is getting too many searches right now – try again in a moment",
                ));
            }
            s => {
                let body = read_json(res).await.unwrap_or(Value::Null);
                let msg = body["message"].as_str().unwrap_or_default().to_string();
                if msg.to_lowercase().contains("credit") {
                    return Err(Error::InsufficientResources("your Serper searches are used up – top up at serper.dev or switch to Wikipedia".into()));
                }
                return Err(Error::unavailable(format!(
                    "Serper answered HTTP {}: {msg}",
                    s.as_u16()
                )));
            }
        }
        let v = read_json(res).await?;
        Ok(parse_serper(&v))
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().trim().to_string()
}

/// The parts of a Serper answer that carry information.
pub fn parse_serper(v: &Value) -> SerperAnswer {
    let mut boxes = Vec::new();
    let ab = &v["answerBox"];
    if ab.is_object() {
        let text = [s(&ab["answer"]), s(&ab["snippet"])]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" – ");
        if !text.is_empty() {
            boxes.push(Page {
                title: s(&ab["title"]),
                url: s(&ab["link"]),
                text,
            });
        }
    }
    let kg = &v["knowledgeGraph"];
    if kg.is_object() {
        let mut lines = vec![s(&kg["description"])];
        if let Some(attrs) = kg["attributes"].as_object() {
            lines.extend(attrs.iter().map(|(k, v)| format!("{k}: {}", s(v))));
        }
        let text = lines
            .into_iter()
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if !text.is_empty() {
            boxes.push(Page {
                title: s(&kg["title"]),
                url: s(&kg["descriptionLink"]),
                text,
            });
        }
    }
    let organic = v["organic"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            let url = s(&o["link"]);
            (!url.is_empty()).then(|| Page {
                title: s(&o["title"]),
                text: [s(&o["date"]), s(&o["snippet"])]
                    .into_iter()
                    .filter(|t| !t.is_empty())
                    .collect::<Vec<_>>()
                    .join(" – "),
                url,
            })
        })
        .collect();
    SerperAnswer { boxes, organic }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_language_codes_go_into_a_host_name() {
        assert_eq!(language("DE"), "de");
        assert_eq!(language("nds"), "nds");
        for bad in ["", "d", "evil.example.org/", "de:80", "deutsch", "e1"] {
            assert_eq!(language(bad), "en", "{bad}");
        }
    }

    #[test]
    fn serper_boxes_and_results_are_read() {
        let v = json!({
            "answerBox": {"title": "Oslo", "answer": "728,714", "link": "https://ssb.no/oslo"},
            "knowledgeGraph": {"title": "Oslo", "description": "Capital of Norway", "descriptionLink": "https://en.wikipedia.org/wiki/Oslo", "attributes": {"Population": "728,714 (2026)"}},
            "organic": [{"title": "Oslo – SSB", "link": "https://ssb.no/oslo", "snippet": "Population figures", "date": "Feb 2026"}, {"title": "no link"}]
        });
        let a = parse_serper(&v);
        assert_eq!(a.boxes.len(), 2);
        assert_eq!(a.boxes[0].text, "728,714");
        assert!(a.boxes[1].text.contains("Population: 728,714 (2026)"));
        assert_eq!(a.organic.len(), 1);
        assert_eq!(a.organic[0].text, "Feb 2026 – Population figures");
    }
}
