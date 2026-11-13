// SPDX-License-Identifier: GPL-3.0-or-later
//! The web interface, built into the program. The same page is served by the farm server (for any
//! browser of the farm) and by the local app (for this machine's window).

use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;

const INDEX: &str = include_str!("../ui/index.html");
const APP: &str = include_str!("../ui/app.js");
const STYLE: &str = include_str!("../ui/style.css");
const ICON: &[u8] = include_bytes!("../assets/icon.svg");

pub fn routes<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/", get(|| async { ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], INDEX) }))
        .route("/app.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], APP) }))
        .route("/style.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], STYLE) }))
        .route("/icon.svg", get(|| async { ([(header::CONTENT_TYPE, "image/svg+xml")], ICON).into_response() }))
}
