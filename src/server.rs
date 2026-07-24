use crate::{
    contract_multipliers,
    mark_prices::MarkPriceCache,
    pnl::{self, PnlCalculation, PnlSourceKind},
    strategy_env::read_env_file,
};
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{
    FromRow, PgPool,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::{HashMap, HashSet},
    env,
    net::SocketAddr,
    path::Path,
    process::Command,
    str::FromStr,
    sync::Arc,
};
use tokio::{sync::Mutex, task::JoinSet};
use tower_http::trace::TraceLayer;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "127.0.0.1:4200";
const DEFAULT_DB_HOST: &str = "/var/run/postgresql";
const DEFAULT_DB_NAME: &str = "crypto_nav_manager";
const DEFAULT_DB_USER: &str = "ubuntu";
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    mark_prices: MarkPriceCache,
    fee_rate_syncs: Arc<Mutex<HashSet<String>>>,
    snapshot_sync: Arc<Mutex<()>>,
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

#[derive(Clone, Debug, FromRow)]
struct SnapshotStrategyRecord {
    slug: String,
    host: String,
    config_url: String,
}

#[derive(Debug, FromRow)]
struct SnapshotRecord {
    strategy_slug: String,
    snapshot_ts_ms: i64,
    fetched_at_ms: i64,
    source_url: String,
    payload: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotResponse {
    strategy_slug: String,
    snapshot_ts_ms: i64,
    fetched_at_ms: i64,
    source_url: String,
    payload: Value,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotSyncResult {
    strategy_slug: String,
    stored: bool,
    snapshot_ts_ms: Option<i64>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotSyncResponse {
    requested: usize,
    stored: usize,
    failed: usize,
    results: Vec<SnapshotSyncResult>,
}

#[derive(Debug, FromRow)]
struct FeeRateRecord {
    market: String,
    instrument: String,
    maker_rate: String,
    taker_rate: String,
    fee_tier: Option<String>,
    fee_group: Option<String>,
    effective_at_ms: i64,
    fetched_at_ms: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct FeeRateResponse {
    market: String,
    instrument: String,
    maker_rate: String,
    taker_rate: String,
    fee_tier: Option<String>,
    fee_group: Option<String>,
    effective_at_ms: i64,
    fetched_at_ms: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AccountFeeRatesResponse {
    slug: String,
    display_name: String,
    exchange: String,
    account_mode: String,
    strategy_kind: String,
    sort_order: i32,
    rates: Vec<FeeRateResponse>,
    hidden_rate_count: usize,
    hidden_instrument_count: usize,
}

const BYBIT_DEFAULT_INSTRUMENTS: [&str; 6] = [
    "BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "BNBUSDT", "DOGEUSDT",
];

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
    let mark_prices = MarkPriceCache::start().await;

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/strategies", get(list_strategies))
        .route("/api/fee-rates", get(list_fee_rates))
        .route("/api/fee-rates/{slug}", get(get_account_fee_rates))
        .route("/api/fee-rates/{slug}/sync", post(sync_account_fee_rates))
        .route("/api/snapshots", get(list_latest_snapshots))
        .route("/api/snapshots/sync", post(sync_snapshots))
        .route("/api/snapshots/{slug}", get(get_latest_snapshot))
        .route("/api/strategies/{slug}", get(get_strategy))
        .route("/api/strategies/{slug}/pnl", get(get_strategy_pnl))
        .with_state(AppState {
            pool,
            mark_prices,
            fee_rate_syncs: Arc::new(Mutex::new(HashSet::new())),
            snapshot_sync: Arc::new(Mutex::new(())),
        })
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

async fn sync_snapshots(State(state): State<AppState>) -> Result<Response, ApiError> {
    let Ok(_guard) = state.snapshot_sync.try_lock() else {
        return Ok((
            StatusCode::CONFLICT,
            Json(ErrorResponse {
                error: "snapshot sync already running",
            }),
        )
            .into_response());
    };
    let strategies = sqlx::query_as::<_, SnapshotStrategyRecord>(
        "SELECT slug,host,config_url FROM strategy_envs WHERE enabled ORDER BY sort_order,slug",
    )
    .fetch_all(&state.pool)
    .await?;
    let requested = strategies.len();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut tasks = JoinSet::new();
    for strategy in strategies {
        let client = client.clone();
        tasks.spawn(async move {
            let slug = strategy.slug;
            let result = async {
                let source_url = snapshot_source_url(&strategy.host, &strategy.config_url)?;
                let payload = client
                    .get(&source_url)
                    .send()
                    .await
                    .with_context(|| format!("request snapshot for {slug}"))?
                    .error_for_status()
                    .with_context(|| format!("snapshot status for {slug}"))?
                    .json::<Value>()
                    .await
                    .with_context(|| format!("decode snapshot for {slug}"))?;
                let snapshot_ts_ms = payload
                    .get("ts_ms")
                    .and_then(Value::as_i64)
                    .filter(|value| *value > 0)
                    .with_context(|| format!("snapshot for {slug} has no valid ts_ms"))?;
                Ok::<_, anyhow::Error>((source_url, snapshot_ts_ms, payload))
            }
            .await;
            (slug, result)
        });
    }

    let mut results = Vec::with_capacity(requested);
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((slug, Ok((source_url, snapshot_ts_ms, payload)))) => {
                sqlx::query(
                    r#"INSERT INTO strategy_snapshots
                       (strategy_slug,snapshot_ts_ms,source_url,payload)
                       VALUES ($1,$2,$3,$4)
                       ON CONFLICT (strategy_slug,snapshot_ts_ms) DO UPDATE SET
                           fetched_at=CURRENT_TIMESTAMP,
                           source_url=EXCLUDED.source_url,
                           payload=EXCLUDED.payload"#,
                )
                .bind(&slug)
                .bind(snapshot_ts_ms)
                .bind(source_url)
                .bind(payload)
                .execute(&state.pool)
                .await?;
                results.push(SnapshotSyncResult {
                    strategy_slug: slug,
                    stored: true,
                    snapshot_ts_ms: Some(snapshot_ts_ms),
                    error: None,
                });
            }
            Ok((slug, Err(error))) => results.push(SnapshotSyncResult {
                strategy_slug: slug,
                stored: false,
                snapshot_ts_ms: None,
                error: Some(format!("{error:#}")),
            }),
            Err(error) => results.push(SnapshotSyncResult {
                strategy_slug: "unknown".to_string(),
                stored: false,
                snapshot_ts_ms: None,
                error: Some(format!("snapshot task failed: {error}")),
            }),
        }
    }
    results.sort_by(|left, right| left.strategy_slug.cmp(&right.strategy_slug));
    let stored = results.iter().filter(|result| result.stored).count();
    Ok(Json(SnapshotSyncResponse {
        requested,
        stored,
        failed: requested.saturating_sub(stored),
        results,
    })
    .into_response())
}

fn snapshot_source_url(host: &str, config_url: &str) -> Result<String> {
    let absolute = if config_url.starts_with("http://") || config_url.starts_with("https://") {
        config_url.to_string()
    } else if host == "local" && config_url.starts_with('/') {
        format!("http://127.0.0.1:4191{config_url}")
    } else {
        anyhow::bail!("snapshot config URL is not absolute for host {host}");
    };
    let mut url = reqwest::Url::parse(&absolute).context("parse snapshot config URL")?;
    let path = url.path().trim_end_matches('/');
    let base = path.strip_suffix("/config").unwrap_or(path);
    url.set_path(&format!("{base}/snapshot"));
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

async fn list_latest_snapshots(
    State(state): State<AppState>,
) -> Result<Json<Vec<SnapshotResponse>>, ApiError> {
    let rows = sqlx::query_as::<_, SnapshotRecord>(
        r#"SELECT DISTINCT ON (strategy_slug)
               strategy_slug,snapshot_ts_ms,
               (EXTRACT(EPOCH FROM fetched_at) * 1000)::bigint AS fetched_at_ms,
               source_url,payload
           FROM strategy_snapshots
           ORDER BY strategy_slug,snapshot_ts_ms DESC"#,
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows.into_iter().map(snapshot_response).collect()))
}

async fn get_latest_snapshot(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Response, ApiError> {
    let row = sqlx::query_as::<_, SnapshotRecord>(
        r#"SELECT strategy_slug,snapshot_ts_ms,
                  (EXTRACT(EPOCH FROM fetched_at) * 1000)::bigint AS fetched_at_ms,
                  source_url,payload
           FROM strategy_snapshots
           WHERE strategy_slug=$1
           ORDER BY snapshot_ts_ms DESC LIMIT 1"#,
    )
    .bind(slug)
    .fetch_optional(&state.pool)
    .await?;
    Ok(match row {
        Some(row) => Json(snapshot_response(row)).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    })
}

fn snapshot_response(row: SnapshotRecord) -> SnapshotResponse {
    SnapshotResponse {
        strategy_slug: row.strategy_slug,
        snapshot_ts_ms: row.snapshot_ts_ms,
        fetched_at_ms: row.fetched_at_ms,
        source_url: row.source_url,
        payload: row.payload,
    }
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

    let mut tasks = JoinSet::new();
    for row in rows {
        tasks.spawn_blocking(move || strategy_response(row));
    }
    let mut strategies = Vec::with_capacity(tasks.len());
    while let Some(result) = tasks.join_next().await {
        strategies.push(result.context("join strategy credential check")??);
    }
    strategies.sort_by(|left, right| {
        left.sort_order
            .cmp(&right.sort_order)
            .then_with(|| left.slug.cmp(&right.slug))
    });
    Ok(Json(strategies))
}

async fn list_fee_rates(
    State(state): State<AppState>,
) -> Result<Json<Vec<AccountFeeRatesResponse>>, ApiError> {
    let strategies = sqlx::query_as::<_, StrategyRecord>(
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

    let mut accounts = Vec::with_capacity(strategies.len());
    for strategy in strategies {
        let rates = load_latest_fee_rates(&state.pool, &strategy.db_schema).await?;
        accounts.push(account_fee_rates_response(strategy, rates, true));
    }

    Ok(Json(accounts))
}

async fn get_account_fee_rates(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Response, ApiError> {
    let strategy = sqlx::query_as::<_, StrategyRecord>(
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
    let Some(strategy) = strategy else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "strategy not found",
            }),
        )
            .into_response());
    };
    let rates = load_latest_fee_rates(&state.pool, &strategy.db_schema).await?;
    Ok(Json(account_fee_rates_response(strategy, rates, false)).into_response())
}

fn account_fee_rates_response(
    strategy: StrategyRecord,
    mut rates: Vec<FeeRateResponse>,
    collapse_bybit: bool,
) -> AccountFeeRatesResponse {
    let mut hidden_rate_count = 0;
    let mut hidden_instruments = HashSet::new();
    if collapse_bybit && strategy.exchange == "bybit" {
        rates.retain(|rate| {
            let keep = is_default_bybit_instrument(&rate.instrument);
            if !keep {
                hidden_rate_count += 1;
                hidden_instruments.insert(rate.instrument.clone());
            }
            keep
        });
    }
    AccountFeeRatesResponse {
        display_name: strategy
            .alias
            .clone()
            .unwrap_or_else(|| strategy.slug.clone()),
        slug: strategy.slug,
        exchange: strategy.exchange,
        account_mode: strategy.account_mode,
        strategy_kind: strategy.strategy_kind,
        sort_order: strategy.sort_order,
        rates,
        hidden_rate_count,
        hidden_instrument_count: hidden_instruments.len(),
    }
}

fn is_default_bybit_instrument(instrument: &str) -> bool {
    let compact = instrument
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_uppercase)
        .collect::<String>();
    BYBIT_DEFAULT_INSTRUMENTS.contains(&compact.as_str())
}

async fn sync_account_fee_rates(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Response, ApiError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM strategy_envs WHERE enabled AND slug = $1)",
    )
    .bind(&slug)
    .fetch_one(&state.pool)
    .await?;
    if !exists {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "strategy not found",
            }),
        )
            .into_response());
    }

    {
        let mut syncs = state.fee_rate_syncs.lock().await;
        if !syncs.insert(slug.clone()) {
            return Ok((
                StatusCode::CONFLICT,
                Json(ErrorResponse {
                    error: "fee rate sync already running",
                }),
            )
                .into_response());
        }
    }

    let process_slug = slug.clone();
    let result = match tokio::task::spawn_blocking(move || run_fee_rate_sync(&process_slug)).await {
        Ok(result) => result,
        Err(error) => Err(error).context("join fee rate sync process"),
    };
    state.fee_rate_syncs.lock().await.remove(&slug);
    result?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn run_fee_rate_sync(slug: &str) -> Result<()> {
    let executable = env::current_exe()
        .context("resolve NAV server executable")?
        .with_file_name("sync_fee_rates");
    let output = Command::new(&executable)
        .args(["--strategy", slug])
        .output()
        .with_context(|| format!("run {} for {slug}", executable.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "fee rate sync for {slug} exited with {}: {}",
            output.status,
            stderr.trim()
        );
    }
    Ok(())
}

async fn load_latest_fee_rates(pool: &PgPool, schema: &str) -> Result<Vec<FeeRateResponse>> {
    if !valid_schema(schema) {
        anyhow::bail!("invalid strategy schema: {schema}");
    }
    let table = format!("{schema}.trading_fee_rates");
    let exists: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
        .bind(&table)
        .fetch_one(pool)
        .await?;
    if exists.is_none() {
        return Ok(Vec::new());
    }

    let query = format!(
        r#"
        SELECT market, instrument,
               maker_rate::text AS maker_rate,
               taker_rate::text AS taker_rate,
               fee_tier, NULLIF(fee_group, '') AS fee_group,
               effective_at_ms,
               (EXTRACT(EPOCH FROM fetched_at) * 1000)::bigint AS fetched_at_ms
        FROM {schema}.trading_fee_rates
        WHERE fetched_at = (SELECT MAX(fetched_at) FROM {schema}.trading_fee_rates)
        ORDER BY market, instrument, fee_group
        "#
    );
    let rows = sqlx::query_as::<_, FeeRateRecord>(sqlx::AssertSqlSafe(query.as_str()))
        .fetch_all(pool)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| FeeRateResponse {
            market: row.market,
            instrument: row.instrument,
            maker_rate: row.maker_rate,
            taker_rate: row.taker_rate,
            fee_tier: row.fee_tier,
            fee_group: row.fee_group,
            effective_at_ms: row.effective_at_ms,
            fetched_at_ms: row.fetched_at_ms,
        })
        .collect())
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
        Some(row) => {
            let response = tokio::task::spawn_blocking(move || strategy_response(row))
                .await
                .context("join strategy credential check")??;
            Ok(Json(response).into_response())
        }
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
        &state.mark_prices,
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
    let content = read_env_file(&row.host, env_path);
    let env_exists = content.is_ok();
    let assigned = match content {
        Ok(content) => assigned_env_keys(&content),
        Err(error) => {
            warn!(host = %row.host, path = %row.env_path, %error, "strategy env unavailable");
            HashMap::new()
        }
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

fn valid_schema(schema: &str) -> bool {
    let mut characters = schema.chars();
    characters
        .next()
        .is_some_and(|character| character.is_ascii_lowercase())
        && characters.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
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
    use super::{
        assigned_env_keys, is_default_bybit_instrument, snapshot_source_url, valid_schema,
    };

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

    #[test]
    fn validates_dynamic_strategy_schema() {
        assert!(valid_schema("binance_intra_arb01"));
        assert!(!valid_schema(""));
        assert!(!valid_schema("public.trading_fee_rates"));
        assert!(!valid_schema("fee-rates"));
    }

    #[test]
    fn limits_default_bybit_instruments() {
        for instrument in [
            "BTCUSDT", "ETH-USDT", "sol_usdt", "XRPUSDT", "BNBUSDT", "DOGEUSDT",
        ] {
            assert!(is_default_bybit_instrument(instrument));
        }
        assert!(!is_default_bybit_instrument("ADAUSDT"));
    }

    #[test]
    fn derives_local_and_remote_snapshot_urls_from_config_urls() {
        assert_eq!(
            snapshot_source_url("local", "/intra/binance-intra-arb01/config").unwrap(),
            "http://127.0.0.1:4191/intra/binance-intra-arb01/snapshot"
        );
        assert_eq!(
            snapshot_source_url(
                "sg",
                "http://47.131.162.78:4191/intra/bybit-intra-arb01/config",
            )
            .unwrap(),
            "http://47.131.162.78:4191/intra/bybit-intra-arb01/snapshot"
        );
        assert!(snapshot_source_url("sg", "/intra/bybit-intra-arb01/config").is_err());
    }
}
