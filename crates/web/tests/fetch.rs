//! Fetching pages against a real local server: what may be fetched, what is
//! refused (redirects inside, other content), and where reading stops.

use std::collections::HashMap;
use std::net::IpAddr;

use ancilo_web::net::{self, MAX_PAGE_BYTES, PublicResolver};
use axum::Router;
use axum::http::header;
use axum::response::{IntoResponse, Redirect};
use axum::routing::get;

async fn serve() -> u16 {
    let app = Router::new()
        .route(
            "/page",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                    "<html><body><article><p>Hello from the page.</p></article></body></html>",
                )
            }),
        )
        .route(
            "/big",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/plain")],
                    "x".repeat(MAX_PAGE_BYTES * 2),
                )
            }),
        )
        .route(
            "/pdf",
            get(|| async {
                ([(header::CONTENT_TYPE, "application/pdf")], "%PDF-1.4").into_response()
            }),
        )
        .route(
            "/to-loopback",
            get(|port: axum::extract::State<u16>| async move {
                Redirect::temporary(&format!("http://127.0.0.1:{}/page", *port))
            }),
        )
        .route(
            "/to-localhost",
            get(|port: axum::extract::State<u16>| async move {
                Redirect::temporary(&format!("http://localhost:{}/page", *port))
            }),
        )
        .route(
            "/to-mapped",
            get(|port: axum::extract::State<u16>| async move {
                Redirect::temporary(&format!("http://other.test:{}/page", *port))
            }),
        )
        .route("/loop", get(|| async { Redirect::temporary("/loop") }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app.with_state(port)).await.unwrap() });
    port
}

fn resolver() -> PublicResolver {
    let lo: IpAddr = "127.0.0.1".parse().unwrap();
    PublicResolver::new(HashMap::from([
        ("pages.test".to_string(), lo),
        ("other.test".to_string(), lo),
    ]))
}

// covers: M6-AC-14
#[tokio::test]
async fn pages_come_only_from_where_they_may() {
    let port = serve().await;
    let r = resolver();
    let client = net::page_client(r.clone(), "test");
    let url = |p: &str| format!("http://pages.test:{port}{p}");

    let ok = net::fetch(&client, &r, &url("/page")).await.unwrap();
    assert!(ok.html && ok.body.contains("Hello from the page."));
    // A redirect to another name the configuration maps is followed …
    let moved = net::fetch(&client, &r, &url("/to-mapped")).await.unwrap();
    assert_eq!(moved.url.host_str(), Some("other.test"));
    // … one inside (an address or "localhost") is not.
    for inside in ["/to-loopback", "/to-localhost"] {
        let err = net::fetch(&client, &r, &url(inside)).await.unwrap_err();
        assert!(
            err.contains("redirect") || err.contains("local") || err.contains("public"),
            "{inside}: {err}"
        );
    }
    // The same server under its address is refused before anything is sent.
    let err = net::fetch(&client, &r, &format!("http://127.0.0.1:{port}/page"))
        .await
        .unwrap_err();
    assert!(err.contains("not a public address"), "{err}");
    assert!(
        net::fetch(&client, &r, &url("/loop")).await.is_err(),
        "redirect loop"
    );
    let err = net::fetch(&client, &r, &url("/pdf")).await.unwrap_err();
    assert!(err.contains("not a web page"), "{err}");
    // Reading stops at the limit.
    let big = net::fetch(&client, &r, &url("/big")).await.unwrap();
    assert_eq!(big.body.len(), MAX_PAGE_BYTES);
}

// covers: M6-AC-14
#[tokio::test]
async fn names_not_mapped_never_reach_this_computer() {
    let port = serve().await;
    // Without the mapping, "pages.test" does not exist, and "localhost" is refused.
    let r = PublicResolver::default();
    let client = net::page_client(r.clone(), "test");
    assert!(
        net::fetch(&client, &r, &format!("http://localhost:{port}/page"))
            .await
            .is_err()
    );
    assert!(
        net::fetch(&client, &r, &format!("http://pages.test:{port}/page"))
            .await
            .is_err()
    );
}
