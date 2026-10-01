//! Fixture (bridges gate, axum family = new coverage, reqwest and std::process families).
use axum::routing::get;
use axum::Router;

async fn axum_user() -> &'static str {
    "user"
}

pub fn app() -> Router {
    Router::new().route("/axum/users/:id", get(axum_user))
}

pub fn load() -> Result<reqwest::blocking::Response, reqwest::Error> {
    reqwest::blocking::get("http://svc.local/axum/users/1")
}

pub fn build() -> std::io::Result<std::process::ExitStatus> {
    std::process::Command::new("procs/build.sh").status()
}
