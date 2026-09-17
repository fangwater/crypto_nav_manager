use anyhow::{Context, Result};
use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;
use tracing::error;

pub const SESSION_COOKIE: &str = "nav_session";
const SESSION_TTL_SECS: i64 = 14 * 24 * 3600;
const SESSION_RENEW_BELOW_SECS: i64 = 7 * 24 * 3600;
const DUMMY_PASSWORD_HASH: &str = "$2a$10$abcdefghijklmnopqrstuuOQzKx8wKz6cZFQmJ7u0n5e";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Admin,
    User,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Admin => "admin",
            Role::User => "user",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "admin" => Some(Role::Admin),
            "user" => Some(Role::User),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct AuthUser {
    pub user_id: i64,
    pub username: String,
    pub role: Role,
}

impl AuthUser {
    pub fn is_admin(&self) -> bool {
        self.role == Role::Admin
    }
}

#[derive(Clone)]
struct MemorySession {
    user: AuthUser,
    expires_at_ms: i64,
}

#[derive(Clone)]
pub struct SessionStore {
    pool: PgPool,
    memory: Option<Arc<Mutex<HashMap<String, MemorySession>>>>,
}

impl SessionStore {
    pub fn persistent(pool: PgPool) -> Self {
        Self { pool, memory: None }
    }

    pub fn volatile(pool: PgPool) -> Self {
        Self {
            pool,
            memory: Some(Arc::new(Mutex::new(HashMap::new()))),
        }
    }

    pub async fn create(&self, user: &AuthUser) -> Result<String> {
        let token: String = sqlx::query_scalar("SELECT encode(gen_random_bytes(32), 'hex')")
            .fetch_one(&self.pool)
            .await
            .context("generate session token")?;
        let hash = token_hash(&token);
        match &self.memory {
            Some(sessions) => {
                let now_ms = chrono::Utc::now().timestamp_millis();
                let mut sessions = sessions.lock().await;
                sessions.retain(|_, session| session.expires_at_ms > now_ms);
                sessions.insert(
                    hash,
                    MemorySession {
                        user: user.clone(),
                        expires_at_ms: now_ms + SESSION_TTL_SECS * 1_000,
                    },
                );
            }
            None => {
                sqlx::query("DELETE FROM nav_sessions WHERE expires_at <= CURRENT_TIMESTAMP")
                    .execute(&self.pool)
                    .await
                    .context("drop expired nav sessions")?;
                sqlx::query(
                    r#"INSERT INTO nav_sessions (token_hash, user_id, expires_at)
                       VALUES ($1, $2, CURRENT_TIMESTAMP + ($3 || ' seconds')::interval)"#,
                )
                .bind(&hash)
                .bind(user.user_id)
                .bind(SESSION_TTL_SECS)
                .execute(&self.pool)
                .await
                .context("store nav session")?;
            }
        }
        Ok(token)
    }

    pub async fn resolve(&self, token: &str) -> Result<Option<AuthUser>> {
        let hash = token_hash(token);
        match &self.memory {
            Some(sessions) => {
                let now_ms = chrono::Utc::now().timestamp_millis();
                let mut sessions = sessions.lock().await;
                let Some(session) = sessions.get_mut(&hash) else {
                    return Ok(None);
                };
                if session.expires_at_ms <= now_ms {
                    sessions.remove(&hash);
                    return Ok(None);
                }
                if session.expires_at_ms - now_ms < SESSION_RENEW_BELOW_SECS * 1_000 {
                    session.expires_at_ms = now_ms + SESSION_TTL_SECS * 1_000;
                }
                Ok(Some(session.user.clone()))
            }
            None => {
                #[derive(sqlx::FromRow)]
                struct SessionRow {
                    user_id: i64,
                    username: String,
                    role: String,
                    expires_in_secs: f64,
                }
                let row = sqlx::query_as::<_, SessionRow>(
                    r#"SELECT s.user_id, u.username, u.role,
                              EXTRACT(EPOCH FROM s.expires_at - CURRENT_TIMESTAMP)::float8
                                  AS expires_in_secs
                       FROM nav_sessions s
                       JOIN nav_users u ON u.user_id = s.user_id
                       WHERE s.token_hash = $1"#,
                )
                .bind(&hash)
                .fetch_optional(&self.pool)
                .await
                .context("load nav session")?;
                let Some(row) = row else {
                    return Ok(None);
                };
                if row.expires_in_secs <= 0.0 {
                    sqlx::query("DELETE FROM nav_sessions WHERE token_hash = $1")
                        .bind(&hash)
                        .execute(&self.pool)
                        .await
                        .context("drop expired nav session")?;
                    return Ok(None);
                }
                if row.expires_in_secs < SESSION_RENEW_BELOW_SECS as f64 {
                    sqlx::query(
                        r#"UPDATE nav_sessions
                           SET expires_at = CURRENT_TIMESTAMP + ($2 || ' seconds')::interval
                           WHERE token_hash = $1"#,
                    )
                    .bind(&hash)
                    .bind(SESSION_TTL_SECS)
                    .execute(&self.pool)
                    .await
                    .context("renew nav session")?;
                }
                let Some(role) = Role::parse(&row.role) else {
                    return Ok(None);
                };
                Ok(Some(AuthUser {
                    user_id: row.user_id,
                    username: row.username,
                    role,
                }))
            }
        }
    }

    pub async fn revoke(&self, token: &str) -> Result<()> {
        let hash = token_hash(token);
        match &self.memory {
            Some(sessions) => {
                sessions.lock().await.remove(&hash);
            }
            None => {
                sqlx::query("DELETE FROM nav_sessions WHERE token_hash = $1")
                    .bind(&hash)
                    .execute(&self.pool)
                    .await
                    .context("revoke nav session")?;
            }
        }
        Ok(())
    }

    pub async fn revoke_user_sessions(&self, user_id: i64, keep_token: Option<&str>) -> Result<()> {
        match &self.memory {
            Some(sessions) => {
                let keep_hash = keep_token.map(token_hash);
                sessions.lock().await.retain(|hash, session| {
                    session.user.user_id != user_id || Some(hash) == keep_hash.as_ref()
                });
            }
            None => {
                sqlx::query(
                    r#"DELETE FROM nav_sessions
                       WHERE user_id = $1 AND token_hash <> COALESCE($2, '')"#,
                )
                .bind(user_id)
                .bind(keep_token.map(token_hash))
                .execute(&self.pool)
                .await
                .context("revoke user nav sessions")?;
            }
        }
        Ok(())
    }
}

pub fn valid_username(username: &str) -> bool {
    !username.is_empty()
        && username.len() <= 64
        && username.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '_' | '.' | '-')
        })
}

pub fn valid_password(password: &str) -> bool {
    (8..=128).contains(&password.len())
}

pub async fn verify_password(
    pool: &PgPool,
    username: &str,
    password: &str,
) -> Result<Option<AuthUser>> {
    #[derive(sqlx::FromRow)]
    struct UserRow {
        user_id: i64,
        username: String,
        role: String,
        matched: bool,
    }
    let row = sqlx::query_as::<_, UserRow>(
        r#"SELECT user_id, username, role,
                  (password_hash = crypt($2, password_hash)) AS matched
           FROM nav_users
           WHERE username = $1"#,
    )
    .bind(username)
    .bind(password)
    .fetch_optional(pool)
    .await
    .context("verify nav user password")?;
    let Some(row) = row else {
        sqlx::query("SELECT crypt($1, $2)")
            .bind(password)
            .bind(DUMMY_PASSWORD_HASH)
            .execute(pool)
            .await
            .context("run dummy password check")?;
        return Ok(None);
    };
    if !row.matched {
        return Ok(None);
    }
    let Some(role) = Role::parse(&row.role) else {
        return Ok(None);
    };
    Ok(Some(AuthUser {
        user_id: row.user_id,
        username: row.username,
        role,
    }))
}

fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn session_token(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in header.split(';') {
        let Some((name, value)) = pair.trim().split_once('=') else {
            continue;
        };
        if name.trim() == SESSION_COOKIE && !value.trim().is_empty() {
            return Some(value.trim().to_string());
        }
    }
    None
}

pub fn session_cookie_header(token: &str) -> String {
    format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={SESSION_TTL_SECS}")
}

pub fn clear_cookie_header() -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0")
}

#[derive(Serialize)]
struct AuthErrorBody {
    error: &'static str,
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(AuthErrorBody {
            error: "authentication required",
        }),
    )
        .into_response()
}

pub async fn require_auth(
    State(store): State<SessionStore>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(token) = session_token(request.headers()) else {
        return unauthorized();
    };
    match store.resolve(&token).await {
        Ok(Some(user)) => {
            request.extensions_mut().insert(user);
            next.run(request).await
        }
        Ok(None) => unauthorized(),
        Err(error) => {
            error!(%error, "session resolution failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(AuthErrorBody {
                    error: "internal server error",
                }),
            )
                .into_response()
        }
    }
}
