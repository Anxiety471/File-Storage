//! Password-protected single-admin sessions for personal Drive use.
//! HTTP cookies authenticate browser traffic; DRAWER_TOKEN remains the machine/MCP credential.
use std::{collections::HashMap, sync::Arc, time::{Duration, Instant}};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Clone)]
pub struct AuthGate {
    password_digest: [u8; 32],
    api_token: Option<String>,
    sessions: Arc<Mutex<HashMap<String, Instant>>>,
    secure_cookie: bool,
}

#[derive(Deserialize)]
pub struct LoginBody {
    password: String,
}
#[derive(Serialize)]
pub struct LoginStatus {
    pub mode: &'static str,
    pub authenticated: bool,
    pub user: Option<&'static str>,
}

fn digest(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}
fn equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}
fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::COOKIE)?.to_str().ok()?.split(';').find_map(|entry| {
        entry.trim().strip_prefix("drawer_session=")
    })
}
fn token(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ")
}

impl AuthGate {
    pub fn new(password: String, api_token: Option<String>, secure_cookie: bool) -> Self {
        Self {
            password_digest: digest(&password),
            api_token,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            secure_cookie,
        }
    }

    pub async fn authenticated(&self, headers: &HeaderMap) -> bool {
        if let Some(expected) = &self.api_token
            && token(headers).is_some_and(|candidate| equal(candidate.as_bytes(), expected.as_bytes())) {
                return true;
        }
        let Some(secret) = cookie(headers) else { return false };
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, expiry| *expiry > Instant::now());
        sessions.contains_key(secret)
    }

    fn set_cookie(&self, value: &str, max_age: usize) -> String {
        format!("drawer_session={value}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
            if self.secure_cookie { "; Secure" } else { "" })
    }
}

pub async fn login(State(gate): State<AuthGate>, Json(input): Json<LoginBody>) -> Response {
    if !equal(&digest(&input.password), &gate.password_digest) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // 256 bits of randomness, with expiration enforced server-side as well.
    let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    gate.sessions.lock().await.insert(secret.clone(), Instant::now() + Duration::from_secs(24 * 3600));
    let mut response = Json(LoginStatus { mode: "password", authenticated: true, user: Some("admin") }).into_response();
    if let Ok(value) = gate.set_cookie(&secret, 24 * 3600).parse() {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response.headers_mut().insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

pub async fn me(State(gate): State<AuthGate>, headers: HeaderMap) -> Json<LoginStatus> {
    let authenticated = gate.authenticated(&headers).await;
    Json(LoginStatus { mode: "password", authenticated, user: authenticated.then_some("admin") })
}

pub async fn logout(State(gate): State<AuthGate>, headers: HeaderMap) -> Response {
    if let Some(secret) = cookie(&headers) {
        gate.sessions.lock().await.remove(secret);
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response.headers_mut().insert(header::SET_COOKIE,
        gate.set_cookie("", 0).parse().expect("valid set-cookie"));
    response
}
