use crate::{
    contract_multipliers,
    pnl::{self, PnlCalculation, PnlSourceKind},
};
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{
    FromRow, PgPool,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{collections::HashMap, env, fs, net::SocketAddr, path::Path, str::FromStr};
use tower_http::trace::TraceLayer;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "127.0.0.1:4200";
const DEFAULT_DB_HOST: &str = "/var/run/postgresql";
const DEFAULT_DB_NAME: &str = "crypto_nav_manager";
const DEFAULT_DB_USER: &str = "ubuntu";
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
struct AppState {
    pool: PgPool,
}

#[derive(Debug, FromRow)]
struct StrategyRecord {
    slug: String,
    alias: Option<String>,
    db_schema: String,
    host: String,
    env_path: String,
    csv_output_dir: String,
    st_ms: i64,
    strategy_kind: String,
    exchange: String,
    account_mode: String,
    required_keys: Value,
    config_url: String,
    sort_order: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StrategyResponse {
    slug: String,
    alias: Option<String>,
    display_name: String,
    db_schema: String,
    host: String,
    env_path: String,
    csv_output_dir: String,
    st_ms: i64,
    strategy_kind: String,
    exchange: String,
    account_mode: String,
    config_url: String,
    sort_order: i32,
    env_exists: bool,
    credentials_ready: bool,
    missing_keys: Vec<String>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    strategies: usize,
}

#[derive(Debug, FromRow)]
struct PnlStrategyRecord {
    db_schema: String,
    st_ms: i64,
    exchange: String,
    account_mode: String,
    strategy_kind: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PnlQuery {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    symbols: Option<String>,
    max_points: Option<usize>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
}

struct ApiError(anyhow::Error);

impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error!(error = ?self.0, "API request failed");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "internal server error",
            }),
        )
            .into_response()
    }
}

pub async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("crypto_nav_manager=info,tower_http=info")),
        )
        .init();

    let bind = env::var("CRYPTO_NAV_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
    let bind: SocketAddr = bind.parse().context("invalid CRYPTO_NAV_BIND")?;
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(postgres_options()?)
        .await
        .context("connect PostgreSQL")?;

    MIGRATOR
        .run(&pool)
        .await
        .context("run PostgreSQL migrations")?;
    contract_multipliers::spawn(pool.clone());

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/strategies", get(list_strategies))
        .route("/api/strategies/{slug}", get(get_strategy))
        .route("/api/strategies/{slug}/pnl", get(get_strategy_pnl))
        .with_state(AppState { pool })
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    info!(%bind, "crypto NAV API started with PostgreSQL");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve HTTP")?;
    Ok(())
}

fn postgres_options() -> Result<PgConnectOptions> {
    match env::var("CRYPTO_NAV_DATABASE_URL") {
        Ok(url) => PgConnectOptions::from_str(&url).context("invalid CRYPTO_NAV_DATABASE_URL"),
        Err(env::VarError::NotPresent) => Ok(PgConnectOptions::new()
            .host(DEFAULT_DB_HOST)
            .username(DEFAULT_DB_USER)
            .database(DEFAULT_DB_NAME)),
        Err(error) => Err(error).context("read CRYPTO_NAV_DATABASE_URL"),
    }
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM strategy_envs")
        .fetch_one(&state.pool)
        .await?;
    Ok(Json(HealthResponse {
        status: "ok",
        strategies: count as usize,
    }))
}

async fn list_strategies(
    State(state): State<AppState>,
) -> Result<Json<Vec<StrategyResponse>>, ApiError> {
    let rows = sqlx::query_as::<_, StrategyRecord>(
        r#"
        SELECT slug, alias, db_schema, host, env_path, csv_output_dir,
               st_ms, strategy_kind, exchange, account_mode, required_keys,
               config_url, sort_order
        FROM strategy_envs
        WHERE enabled
        ORDER BY sort_order, slug
        "#,
    )
    .fetch_all(&state.pool)
    .await?;

    rows.into_iter()
        .map(strategy_response)
        .collect::<Result<Vec<_>, _>>()
        .map(Json)
        .map_err(ApiError)
}

async fn get_strategy(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Response, ApiError> {
    let row = sqlx::query_as::<_, StrategyRecord>(
        r#"
        SELECT slug, alias, db_schema, host, env_path, csv_output_dir,
               st_ms, strategy_kind, exchange, account_mode, required_keys,
               config_url, sort_order
        FROM strategy_envs
        WHERE enabled AND slug = $1
        "#,
    )
    .bind(slug)
    .fetch_optional(&state.pool)
    .await?;

    match row {
        Some(row) => Ok(Json(strategy_response(row)?).into_response()),
        None => Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "strategy not found",
            }),
        )
            .into_response()),
    }
}
async fn get_strategy_pnl(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
    Query(query): Query<PnlQuery>,
) -> Result<Response, ApiError> {
    let strategy = sqlx::query_as::<_, PnlStrategyRecord>(
        r#"
        SELECT db_schema, st_ms, exchange, account_mode, strategy_kind
        FROM strategy_envs
        WHERE enabled AND slug = $1
        "#,
    )
    .bind(slug)
    .fetch_optional(&state.pool)
    .await?;
    let Some(strategy) = strategy else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "strategy not found",
            }),
        )
            .into_response());
    };

    let Some(source) = PnlSourceKind::for_strategy(
        &strategy.strategy_kind,
        &strategy.exchange,
        &strategy.account_mode,
    ) else {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: "PnL data source is not available for this strategy",
            }),
        )
            .into_response());
    };

    let start_ms = query.start_ms.unwrap_or(strategy.st_ms);
    let end_ms = query
        .end_ms
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    if start_ms < strategy.st_ms {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "startMs must be greater than or equal to strategy stMs",
            }),
        )
            .into_response());
    }
    if end_ms < start_ms {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "endMs must be greater than or equal to startMs",
            }),
        )
            .into_response());
    }

    let selected_symbols = query
        .symbols
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>();
    let inputs = pnl::load_inputs(
        &state.pool,
        source,
        &strategy.db_schema,
        &strategy.exchange,
        strategy.st_ms,
        end_ms,
    )
    .await?;
    let response = pnl::calculate(
        inputs,
        PnlCalculation {
            source,
            exchange: strategy.exchange,
            strategy_start_ms: strategy.st_ms,
            start_ms,
            end_ms,
            selected_symbols,
            max_points: query.max_points.unwrap_or(3_000).clamp(200, 10_000),
        },
    )?;

    Ok(Json(response).into_response())
}

fn strategy_response(row: StrategyRecord) -> Result<StrategyResponse> {
    let required_keys: Vec<String> =
        serde_json::from_value(row.required_keys).context("decode required_keys")?;
    let display_name = row.alias.clone().unwrap_or_else(|| row.slug.clone());
    let env_path = Path::new(&row.env_path);
    let env_exists = row.host == "local" && env_path.is_file();
    let assigned = if env_exists {
        fs::read_to_string(env_path)
            .map(|content| assigned_env_keys(&content))
            .unwrap_or_default()
    } else {
        HashMap::new()
    };
    let missing_keys = required_keys
        .into_iter()
        .filter(|key| !assigned.get(key).copied().unwrap_or(false))
        .collect::<Vec<_>>();

    Ok(StrategyResponse {
        slug: row.slug,
        alias: row.alias,
        display_name,
        db_schema: row.db_schema,
        host: row.host,
        env_path: row.env_path,
        csv_output_dir: row.csv_output_dir,
        st_ms: row.st_ms,
        strategy_kind: row.strategy_kind,
        exchange: row.exchange,
        account_mode: row.account_mode,
        config_url: row.config_url,
        sort_order: row.sort_order,
        env_exists,
        credentials_ready: env_exists && missing_keys.is_empty(),
        missing_keys,
    })
}

fn assigned_env_keys(content: &str) -> HashMap<String, bool> {
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim();
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty()
                || !key
                    .chars()
                    .all(|character| character == '_' || character.is_ascii_alphanumeric())
            {
                return None;
            }
            let value = value.trim();
            let assigned = !value.is_empty() && value != "\"\"" && value != "''";
            Some((key.to_string(), assigned))
        })
        .collect()
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::assigned_env_keys;

    #[test]
    fn detects_only_non_empty_assignments() {
        let keys = assigned_env_keys(
            r#"
            # secret values are never returned
            export BINANCE_API_KEY="abc"
            BINANCE_API_SECRET=''
            INVALID-LINE=value
            IPC_NAMESPACE=binance_fr_arb01
            "#,
        );

        assert_eq!(keys.get("BINANCE_API_KEY"), Some(&true));
        assert_eq!(keys.get("BINANCE_API_SECRET"), Some(&false));
        assert_eq!(keys.get("IPC_NAMESPACE"), Some(&true));
        assert!(!keys.contains_key("INVALID-LINE"));
        assert!(!format!("{keys:?}").contains("abc"));
    }
}
