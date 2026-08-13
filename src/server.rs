use crate::{
    account_risk::{AccountRiskCache, AccountRiskFeed, AccountRiskSnapshot},
    binance_premium_index, bybit_premium_index, contract_multipliers,
    fr_position_limits::{FrLimitSource, FrPositionLimitMonitor, FrPositionLimitOverview},
    intra_analysis::{
        self, ArbDirection, IntraAnalysisFeeEvent, IntraAnalysisFundingEvent, IntraAnalysisOrder,
        IntraAnalysisRequest, PremiumIndexCandle,
    },
    live_history,
    mark_prices::MarkPriceCache,
    pnl::{self, InitialPosition, PnlCalculation, PnlSourceKind},
    postgres, runtime,
    strategy_env::read_env_file,
};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool, migrate::Migrator, postgres::PgConnectOptions};
use std::{
    collections::{HashMap, HashSet},
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::Command,
    str::FromStr,
    sync::Arc,
};
use tokio::{sync::Mutex, task::JoinSet};
use tower_http::{
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const DEFAULT_BIND: &str = "127.0.0.1:4200";
const DEFAULT_DB_HOST: &str = "/var/run/postgresql";
const DEFAULT_DB_NAME: &str = "crypto_nav_manager";
const DEFAULT_DB_USER: &str = "ubuntu";
const FRONTEND_DIR_ENV: &str = "CRYPTO_NAV_FRONTEND_DIR";
static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Clone, Copy)]
struct IntraAnalysisAdapter {
    premium_table: &'static str,
    premium_adapter: &'static str,
}

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    read_only: bool,
    account_risks: AccountRiskCache,
    mark_prices: MarkPriceCache,
    fee_rate_syncs: Arc<Mutex<HashSet<String>>>,
    snapshot_sync: Arc<Mutex<()>>,
    fr_position_limits: FrPositionLimitMonitor,
}

#[derive(Clone, Debug, FromRow)]
struct FrLimitStrategyRecord {
    slug: String,
    host: String,
    env_path: String,
    config_url: String,
    exchange: String,
}

#[derive(Debug, FromRow)]
struct AccountRiskFeedRecord {
    slug: String,
    exchange: String,
    sort_order: i32,
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
#[serde(rename_all = "camelCase")]
struct HealthResponse {
    status: &'static str,
    strategies: usize,
    read_only: bool,
}

#[derive(Debug, FromRow)]
struct PnlStrategyRecord {
    db_schema: String,
    st_ms: i64,
    exchange: String,
    account_mode: String,
    strategy_kind: String,
}

#[derive(Debug, FromRow)]
struct IntraAnalysisStrategyRecord {
    slug: String,
    alias: Option<String>,
    db_schema: String,
    st_ms: i64,
    exchange: String,
    strategy_kind: String,
}

#[derive(Debug, FromRow)]
struct IntraAnalysisOrderRecord {
    fkey: i64,
    symbol: String,
    side: String,
    event_ts_us: i64,
    spot_price: f64,
    futures_price: f64,
    quantity: f64,
    premium_open_rate: Option<f64>,
    premium_high_rate: Option<f64>,
    premium_low_rate: Option<f64>,
    premium_close_rate: Option<f64>,
}

#[derive(Debug, FromRow)]
struct IntraAnalysisFeeRecord {
    symbol: String,
    ts: i64,
    notional_usdt: f64,
    fee_usdt: Option<f64>,
}

#[derive(Debug, FromRow)]
struct IntraAnalysisFundingRecord {
    symbol: String,
    ts: i64,
    amount_usdt: f64,
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
struct SnapshotSummaryResponse {
    strategy_slug: String,
    snapshot_ts_ms: i64,
    fetched_at_ms: i64,
    source_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetInitialSnapshotRequest {
    snapshot_ts_ms: i64,
}

struct ParsedInitialPositions {
    positions: Vec<InitialPosition>,
    skipped_count: usize,
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

#[derive(Clone, Debug, FromRow)]
struct HistorySyncStrategyRecord {
    slug: String,
    host: String,
    exchange: String,
    strategy_kind: String,
}

#[derive(Debug, FromRow)]
struct HistorySyncWatermarkRecord {
    strategy_slug: String,
    dataset: String,
    success_end_ms: i64,
    fetched_at_ms: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistorySyncDatasetResponse {
    dataset: &'static str,
    success_end_ms: Option<i64>,
    fetched_at_ms: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistorySyncStatusResponse {
    strategy_slug: String,
    scheduled: bool,
    last_fetched_at_ms: Option<i64>,
    datasets: Vec<HistorySyncDatasetResponse>,
}

#[derive(Debug, FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
struct AlignmentStatusResponse {
    strategy_slug: String,
    state: String,
    phase: String,
    progress_percent: i32,
    automatic_enabled: bool,
    started_at_ms: Option<i64>,
    updated_at_ms: i64,
    completed_at_ms: Option<i64>,
    candidate_end_ms: Option<i64>,
    scan_start_ms: Option<i64>,
    pg_success_end_ms: Option<i64>,
    actual_end_ms: Option<i64>,
    group_count: Option<i32>,
    mismatch_count: Option<i32>,
    pg_event_count: Option<i64>,
    local_event_count: Option<i64>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetAlignmentAutomaticRequest {
    enabled: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AlignmentAutomaticResponse {
    strategy_slug: String,
    automatic_enabled: bool,
}

#[derive(Debug, FromRow)]
struct IntraMatchingStrategyRecord {
    slug: String,
    alias: Option<String>,
    db_schema: String,
    exchange: String,
}

#[derive(Debug, FromRow)]
struct IntraMatchingWatermarkRecord {
    source_read_through_us: i64,
    events_released_through_us: i64,
    margin_finalized_through_us: i64,
    verified_through_ms: i64,
    reorder_window_us: i64,
    updated_at_ms: i64,
}

#[derive(Debug, FromRow)]
struct IntraMatchingCountRecord {
    total_orders: i64,
    pending_orders: i64,
    completed_orders: i64,
    netted_orders: i64,
    mixed_orders: i64,
    pending_fill_amount: f64,
    pending_remaining_amount: f64,
    pending_notional: f64,
    last_order_updated_at_ms: Option<i64>,
}

#[derive(Debug, FromRow)]
struct IntraHedgeCountRecord {
    total_hedges: i64,
    unallocated_hedges: i64,
    unallocated_amount: f64,
    anchor_misses: i64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IntraMatchingSummaryResponse {
    strategy_slug: String,
    display_name: String,
    exchange: String,
    source_read_through_us: i64,
    events_released_through_us: i64,
    margin_finalized_through_us: i64,
    verified_through_ms: i64,
    reorder_window_us: i64,
    updated_at_ms: i64,
    total_orders: i64,
    pending_orders: i64,
    completed_orders: i64,
    netted_orders: i64,
    mixed_orders: i64,
    pending_fill_amount: f64,
    pending_remaining_amount: f64,
    pending_notional: f64,
    total_hedges: i64,
    unallocated_hedges: i64,
    unallocated_amount: f64,
    anchor_misses: i64,
    last_order_updated_at_ms: Option<i64>,
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

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct IntraAnalysisQuery {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    symbols: Option<String>,
    reference_fee_bps: Option<f64>,
    max_points: Option<usize>,
    max_matches: Option<usize>,
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
    let read_only = runtime::read_only()?;
    let pool = postgres::pool_options(5, read_only)
        .connect_with(postgres_options()?)
        .await
        .context("connect PostgreSQL")?;

    let (account_risks, mark_prices) = if read_only {
        info!("read-only mode enabled; migrations and background data sources disabled");
        (AccountRiskCache::default(), MarkPriceCache::default())
    } else {
        MIGRATOR
            .run(&pool)
            .await
            .context("run PostgreSQL migrations")?;
        let account_risk_feeds = sqlx::query_as::<_, AccountRiskFeedRecord>(
            r#"SELECT slug,exchange,sort_order
               FROM strategy_envs
               WHERE enabled
                 AND host = 'local'
                 AND account_mode IN ('portfolio_margin','unified')
                 AND strategy_kind <> 'market_making'
               ORDER BY sort_order,slug"#,
        )
        .fetch_all(&pool)
        .await
        .context("load local account risk feeds")?
        .into_iter()
        .filter_map(|row| AccountRiskFeed::new(row.slug, row.exchange, row.sort_order))
        .collect();
        let account_risks = AccountRiskCache::start(account_risk_feeds);
        contract_multipliers::spawn(pool.clone());
        binance_premium_index::spawn(pool.clone());
        bybit_premium_index::spawn(pool.clone());
        live_history::spawn(pool.clone())?;
        let mark_prices = MarkPriceCache::start().await;
        (account_risks, mark_prices)
    };
    let fr_position_limits = FrPositionLimitMonitor::new()?;

    let mut app = Router::new()
        .route("/api/health", get(health))
        .route("/api/strategies", get(list_strategies))
        .route("/api/account-risks", get(list_account_risks))
        .route("/api/fr-position-limits", get(list_fr_position_limits))
        .route("/api/history-sync-status", get(list_history_sync_status))
        .route("/api/alignment-status", get(list_alignment_status))
        .route("/api/intra-matching", get(list_intra_matching))
        .route("/api/analysis/{slug}/intra-fifo", get(get_intra_analysis))
        .route("/api/fee-rates", get(list_fee_rates))
        .route("/api/fee-rates/{slug}", get(get_account_fee_rates))
        .route("/api/snapshots", get(list_latest_snapshots))
        .route(
            "/api/snapshots/{slug}/history",
            get(list_strategy_snapshots),
        )
        .route("/api/snapshots/{slug}", get(get_latest_snapshot))
        .route("/api/strategies/{slug}", get(get_strategy))
        .route(
            "/api/strategies/{slug}/initial-snapshot",
            get(get_initial_snapshot),
        )
        .route("/api/strategies/{slug}/pnl", get(get_strategy_pnl));
    if !read_only {
        app = app
            .route(
                "/api/alignment-status/{slug}",
                axum::routing::put(set_alignment_automatic),
            )
            .route("/api/fee-rates/{slug}/sync", post(sync_account_fee_rates))
            .route("/api/snapshots/sync", post(sync_snapshots))
            .route(
                "/api/strategies/{slug}/initial-snapshot",
                axum::routing::put(set_initial_snapshot).delete(clear_initial_snapshot),
            );
    }
    if let Some(frontend_dir) = frontend_dir()? {
        let index = frontend_dir.join("index.html");
        info!(path = %frontend_dir.display(), "serving NAV frontend");
        app = app
            .route("/", get(|| async { Redirect::temporary("/nav/") }))
            .nest_service(
                "/nav",
                ServeDir::new(frontend_dir).not_found_service(ServeFile::new(index)),
            );
    }
    let app = app
        .with_state(AppState {
            pool,
            read_only,
            account_risks,
            mark_prices,
            fee_rate_syncs: Arc::new(Mutex::new(HashSet::new())),
            snapshot_sync: Arc::new(Mutex::new(())),
            fr_position_limits,
        })
        .layer(TraceLayer::new_for_http());

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind {bind}"))?;
    info!(%bind, read_only, "crypto NAV API started with PostgreSQL");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve HTTP")?;
    Ok(())
}

fn frontend_dir() -> Result<Option<PathBuf>> {
    let Some(value) = env::var_os(FRONTEND_DIR_ENV) else {
        return Ok(None);
    };
    if value.is_empty() {
        bail!("{FRONTEND_DIR_ENV} must not be empty");
    }
    let directory = PathBuf::from(value);
    if !directory.join("index.html").is_file() {
        bail!(
            "{FRONTEND_DIR_ENV} does not contain index.html: {}",
            directory.display()
        );
    }
    Ok(Some(directory))
}

async fn list_account_risks(State(state): State<AppState>) -> Json<Vec<AccountRiskSnapshot>> {
    Json(state.account_risks.snapshots())
}

async fn list_fr_position_limits(
    State(state): State<AppState>,
) -> Result<Json<FrPositionLimitOverview>, ApiError> {
    let strategies = sqlx::query_as::<_, FrLimitStrategyRecord>(
        r#"SELECT slug,host,env_path,config_url,exchange
           FROM strategy_envs
           WHERE enabled
             AND slug IN (
               'binance_fr_arb03',
               'binance_fr_arb04',
               'gate_fr_arb01',
               'gate_fr_arb02'
             )
           ORDER BY CASE slug
             WHEN 'binance_fr_arb03' THEN 1
             WHEN 'binance_fr_arb04' THEN 2
             WHEN 'gate_fr_arb01' THEN 3
             WHEN 'gate_fr_arb02' THEN 4
             ELSE 99
           END"#,
    )
    .fetch_all(&state.pool)
    .await?;

    let mut sources = Vec::with_capacity(strategies.len());
    for (sort_order, strategy) in strategies.into_iter().enumerate() {
        sources.push(FrLimitSource {
            strategy_slug: strategy.slug,
            host: strategy.host.clone(),
            env_path: strategy.env_path,
            exchange: strategy.exchange,
            snapshot_url: snapshot_source_url(&strategy.host, &strategy.config_url)?,
            sort_order,
        });
    }

    Ok(Json(state.fr_position_limits.overview(sources).await))
}

async fn list_history_sync_status(
    State(state): State<AppState>,
) -> Result<Json<Vec<HistorySyncStatusResponse>>, ApiError> {
    let strategies = sqlx::query_as::<_, HistorySyncStrategyRecord>(
        r#"SELECT slug,host,exchange,strategy_kind
           FROM strategy_envs
           WHERE enabled
           ORDER BY sort_order,slug"#,
    )
    .fetch_all(&state.pool)
    .await?;
    let watermarks = sqlx::query_as::<_, HistorySyncWatermarkRecord>(
        r#"SELECT strategy_slug,dataset,success_end_ms,
                  (EXTRACT(EPOCH FROM updated_at) * 1000)::bigint AS fetched_at_ms
           FROM history_sync_watermarks"#,
    )
    .fetch_all(&state.pool)
    .await?;
    let mut watermarks_by_strategy: HashMap<String, HashMap<String, (i64, i64)>> = HashMap::new();
    for watermark in watermarks {
        watermarks_by_strategy
            .entry(watermark.strategy_slug)
            .or_default()
            .insert(
                watermark.dataset,
                (watermark.success_end_ms, watermark.fetched_at_ms),
            );
    }

    Ok(Json(
        strategies
            .into_iter()
            .map(|strategy| {
                let expected = expected_history_datasets(
                    &strategy.slug,
                    &strategy.host,
                    &strategy.exchange,
                    &strategy.strategy_kind,
                );
                let watermarks = watermarks_by_strategy.get(&strategy.slug);
                let datasets = expected
                    .unwrap_or_default()
                    .iter()
                    .map(|dataset| {
                        let watermark = watermarks.and_then(|items| items.get(*dataset));
                        HistorySyncDatasetResponse {
                            dataset,
                            success_end_ms: watermark.map(|value| value.0),
                            fetched_at_ms: watermark.map(|value| value.1),
                        }
                    })
                    .collect::<Vec<_>>();
                let last_fetched_at_ms = (!datasets.is_empty()
                    && datasets
                        .iter()
                        .all(|dataset| dataset.fetched_at_ms.is_some()))
                .then(|| {
                    datasets
                        .iter()
                        .filter_map(|dataset| dataset.fetched_at_ms)
                        .min()
                        .expect("non-empty complete history sync datasets")
                });
                HistorySyncStatusResponse {
                    strategy_slug: strategy.slug,
                    scheduled: expected.is_some(),
                    last_fetched_at_ms,
                    datasets,
                }
            })
            .collect(),
    ))
}

async fn list_alignment_status(
    State(state): State<AppState>,
) -> Result<Json<Vec<AlignmentStatusResponse>>, ApiError> {
    let rows = sqlx::query_as::<_, AlignmentStatusResponse>(
        r#"SELECT a.strategy_slug,a.state,a.phase,a.progress_percent,a.automatic_enabled,
                  (EXTRACT(EPOCH FROM a.started_at) * 1000)::bigint AS started_at_ms,
                  (EXTRACT(EPOCH FROM a.updated_at) * 1000)::bigint AS updated_at_ms,
                  (EXTRACT(EPOCH FROM a.completed_at) * 1000)::bigint AS completed_at_ms,
                  a.candidate_end_ms,a.scan_start_ms,a.pg_success_end_ms,a.actual_end_ms,
                  a.group_count,a.mismatch_count,a.pg_event_count,a.local_event_count,a.message
           FROM rocksdb_alignment_status a
           JOIN strategy_envs s ON s.slug=a.strategy_slug
           WHERE s.enabled
           ORDER BY s.sort_order,s.slug"#,
    )
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(rows))
}

async fn set_alignment_automatic(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
    Json(request): Json<SetAlignmentAutomaticRequest>,
) -> Result<Response, ApiError> {
    let updated = sqlx::query_as::<_, (String, bool)>(
        r#"UPDATE rocksdb_alignment_status
           SET automatic_enabled=$2,automatic_updated_at=CURRENT_TIMESTAMP
           WHERE strategy_slug=$1
           RETURNING strategy_slug,automatic_enabled"#,
    )
    .bind(&slug)
    .bind(request.enabled)
    .fetch_optional(&state.pool)
    .await?;

    Ok(match updated {
        Some((strategy_slug, automatic_enabled)) => Json(AlignmentAutomaticResponse {
            strategy_slug,
            automatic_enabled,
        })
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "alignment strategy not found",
            }),
        )
            .into_response(),
    })
}

async fn list_intra_matching(
    State(state): State<AppState>,
) -> Result<Json<Vec<IntraMatchingSummaryResponse>>, ApiError> {
    let strategies = sqlx::query_as::<_, IntraMatchingStrategyRecord>(
        r#"SELECT slug,alias,db_schema,exchange
           FROM strategy_envs
           WHERE enabled
             AND slug IN (
                 'binance-intra-arb01',
                 'bybit-intra-arb01',
                 'bybit-intra-arb02'
             )
           ORDER BY sort_order,slug"#,
    )
    .fetch_all(&state.pool)
    .await?;

    let mut summaries = Vec::with_capacity(strategies.len());
    for strategy in strategies {
        if !valid_schema(&strategy.db_schema) {
            return Err(anyhow::anyhow!("invalid strategy schema: {}", strategy.db_schema).into());
        }
        let watermark_query = format!(
            "SELECT source_read_through_us,events_released_through_us,
             margin_finalized_through_us,verified_through_ms,reorder_window_us,
             (EXTRACT(EPOCH FROM updated_at) * 1000)::bigint AS updated_at_ms
             FROM {}.intra_match_watermark WHERE singleton",
            strategy.db_schema
        );
        let watermark = sqlx::query_as::<_, IntraMatchingWatermarkRecord>(sqlx::AssertSqlSafe(
            watermark_query.as_str(),
        ))
        .fetch_one(&state.pool)
        .await?;

        let counts_query = format!(
            "SELECT COUNT(*)::bigint AS total_orders,
             COUNT(*) FILTER (WHERE matching_state='pending')::bigint AS pending_orders,
             COUNT(*) FILTER (WHERE matching_state='completed')::bigint AS completed_orders,
             COUNT(*) FILTER (WHERE matching_state='netted')::bigint AS netted_orders,
             COUNT(*) FILTER (WHERE matching_state='mixed')::bigint AS mixed_orders,
             COALESCE(SUM(open_fill_amount) FILTER (WHERE matching_state='pending'),0)::double precision AS pending_fill_amount,
             COALESCE(SUM(remaining_amount) FILTER (WHERE matching_state='pending'),0)::double precision AS pending_remaining_amount,
             COALESCE(SUM(remaining_amount * price) FILTER (WHERE matching_state='pending'),0)::double precision AS pending_notional,
             (EXTRACT(EPOCH FROM MAX(updated_at)) * 1000)::bigint AS last_order_updated_at_ms
             FROM {}.intra_orders",
            strategy.db_schema
        );
        let counts = sqlx::query_as::<_, IntraMatchingCountRecord>(sqlx::AssertSqlSafe(
            counts_query.as_str(),
        ))
        .fetch_one(&state.pool)
        .await?;

        let hedge_query = format!(
            "SELECT COUNT(*)::bigint AS total_hedges,
             COUNT(*) FILTER (WHERE unallocated_amount > 1e-8)::bigint AS unallocated_hedges,
             COALESCE(SUM(unallocated_amount),0)::double precision AS unallocated_amount,
             COUNT(*) FILTER (WHERE main_fkey IS NOT NULL AND NOT anchor_matched)::bigint AS anchor_misses
             FROM {}.intra_hedges",
            strategy.db_schema
        );
        let hedges =
            sqlx::query_as::<_, IntraHedgeCountRecord>(sqlx::AssertSqlSafe(hedge_query.as_str()))
                .fetch_one(&state.pool)
                .await?;

        let display_name = strategy
            .alias
            .filter(|alias| !alias.trim().is_empty())
            .unwrap_or_else(|| strategy.slug.clone());
        summaries.push(IntraMatchingSummaryResponse {
            strategy_slug: strategy.slug,
            display_name,
            exchange: strategy.exchange,
            source_read_through_us: watermark.source_read_through_us,
            events_released_through_us: watermark.events_released_through_us,
            margin_finalized_through_us: watermark.margin_finalized_through_us,
            verified_through_ms: watermark.verified_through_ms,
            reorder_window_us: watermark.reorder_window_us,
            updated_at_ms: watermark.updated_at_ms,
            total_orders: counts.total_orders,
            pending_orders: counts.pending_orders,
            completed_orders: counts.completed_orders,
            netted_orders: counts.netted_orders,
            mixed_orders: counts.mixed_orders,
            pending_fill_amount: counts.pending_fill_amount,
            pending_remaining_amount: counts.pending_remaining_amount,
            pending_notional: counts.pending_notional,
            total_hedges: hedges.total_hedges,
            unallocated_hedges: hedges.unallocated_hedges,
            unallocated_amount: hedges.unallocated_amount,
            anchor_misses: hedges.anchor_misses,
            last_order_updated_at_ms: counts.last_order_updated_at_ms,
        });
    }
    Ok(Json(summaries))
}

async fn get_intra_analysis(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
    Query(query): Query<IntraAnalysisQuery>,
) -> Result<Response, ApiError> {
    let strategy = sqlx::query_as::<_, IntraAnalysisStrategyRecord>(
        r#"SELECT slug,alias,db_schema,st_ms,exchange,strategy_kind
           FROM strategy_envs
           WHERE enabled AND slug=$1"#,
    )
    .bind(&slug)
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
    let Some(adapter) = intra_analysis_adapter(&strategy) else {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: "combination FIFO analysis is not available for this strategy",
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
                error: "startMs must not be earlier than the strategy start",
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
    let reference_fee_bps = query.reference_fee_bps.unwrap_or(1.0);
    if !reference_fee_bps.is_finite() || reference_fee_bps.abs() > 100.0 {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "referenceFeeBps must be between -100 and 100",
            }),
        )
            .into_response());
    }
    if !valid_schema(&strategy.db_schema) {
        return Err(anyhow::anyhow!("invalid strategy schema: {}", strategy.db_schema).into());
    }

    let order_sql = format!(
        r#"WITH hedge_legs AS (
               SELECT h.main_fkey,
                      COALESCE(h.fill_ts_us,h.update_ts_us) AS event_ts_us,
                      h.amount,h.cprice,
                      p.open_rate,p.high_rate,p.low_rate,p.close_rate
               FROM {0}.intra_hedges h
               JOIN {0}.intra_orders anchor
                 ON anchor.fkey=h.main_fkey
                AND anchor.symbol=h.symbol
                AND anchor.side<>h.side
               LEFT JOIN {1} p
                 ON p.symbol=upper(h.symbol)
                AND p.interval='1m'
                AND p.open_time_ms=(COALESCE(h.fill_ts_us,h.update_ts_us) / 60000000) * 60000
               WHERE h.cprice IS NOT NULL AND h.amount>1e-10
           ), hedge_prices AS (
               SELECT main_fkey,
                      max(event_ts_us) AS event_ts_us,
                      sum(amount * cprice) / NULLIF(sum(amount),0) AS futures_price,
                      sum(amount) AS hedge_quantity,
                      CASE WHEN count(open_rate)=count(*)
                           THEN sum(amount * open_rate) / NULLIF(sum(amount),0) END AS premium_open_rate,
                      CASE WHEN count(high_rate)=count(*)
                           THEN sum(amount * high_rate) / NULLIF(sum(amount),0) END AS premium_high_rate,
                      CASE WHEN count(low_rate)=count(*)
                           THEN sum(amount * low_rate) / NULLIF(sum(amount),0) END AS premium_low_rate,
                      CASE WHEN count(close_rate)=count(*)
                           THEN sum(amount * close_rate) / NULLIF(sum(amount),0) END AS premium_close_rate
               FROM hedge_legs
               GROUP BY main_fkey
           )
           SELECT o.fkey,o.symbol,o.side,h.event_ts_us,
                  o.price::float8 AS spot_price,h.futures_price::float8,
                  LEAST(o.open_fill_amount,h.hedge_quantity)::float8 AS quantity,
                  h.premium_open_rate,h.premium_high_rate,
                  h.premium_low_rate,h.premium_close_rate
           FROM {0}.intra_orders o
           JOIN hedge_prices h ON h.main_fkey=o.fkey
           WHERE LEAST(o.open_fill_amount,h.hedge_quantity)>1e-10
             AND h.event_ts_us >= $1
             AND h.event_ts_us < $2
           ORDER BY event_ts_us,fkey"#,
        strategy.db_schema, adapter.premium_table
    );
    let load_start_us = strategy.st_ms.saturating_mul(1_000);
    let load_end_us = end_ms.saturating_add(1).saturating_mul(1_000);
    let rows = sqlx::query_as::<_, IntraAnalysisOrderRecord>(sqlx::AssertSqlSafe(order_sql))
        .bind(load_start_us)
        .bind(load_end_us)
        .fetch_all(&state.pool)
        .await?;
    let orders = rows
        .into_iter()
        .map(|row| {
            Ok(IntraAnalysisOrder {
                fkey: row.fkey,
                symbol: row.symbol,
                direction: ArbDirection::from_margin_side(&row.side)?,
                completed_at_ms: row.event_ts_us / 1_000,
                spot_price: row.spot_price,
                futures_price: row.futures_price,
                quantity: row.quantity,
                premium: match (
                    row.premium_open_rate,
                    row.premium_high_rate,
                    row.premium_low_rate,
                    row.premium_close_rate,
                ) {
                    (Some(open_rate), Some(high_rate), Some(low_rate), Some(close_rate)) => {
                        Some(PremiumIndexCandle {
                            open_rate,
                            high_rate,
                            low_rate,
                            close_rate,
                        })
                    }
                    _ => None,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let fee_sql = format!(
        r#"SELECT upper(symbol) AS symbol,event_time_ms AS ts,
                  COALESCE(quote_quantity,price * quantity)::float8 AS notional_usdt,
                  CASE
                    WHEN upper(COALESCE(fee_asset,'')) IN ('USD','USDC','USDT')
                      THEN fee_amount::float8
                    WHEN upper(COALESCE(fee_asset,'')) =
                         regexp_replace(upper(symbol),'(?:USDT|USDC|USD)$','')
                      THEN (fee_amount * price)::float8
                    ELSE fee_usdt::float8
                  END AS fee_usdt
           FROM {}.trades
           WHERE event_time_ms >= $1 AND event_time_ms <= $2
           ORDER BY event_time_ms,symbol,trade_id"#,
        strategy.db_schema
    );
    let fee_events = sqlx::query_as::<_, IntraAnalysisFeeRecord>(sqlx::AssertSqlSafe(fee_sql))
        .bind(start_ms)
        .bind(end_ms)
        .fetch_all(&state.pool)
        .await?
        .into_iter()
        .map(|row| IntraAnalysisFeeEvent {
            symbol: row.symbol,
            ts: row.ts,
            notional_usdt: row.notional_usdt,
            fee_usdt: row.fee_usdt,
        })
        .collect::<Vec<_>>();
    let funding_sql = format!(
        r#"SELECT upper(symbol) AS symbol,event_time_ms AS ts,
                  COALESCE(amount_usdt,amount)::float8 AS amount_usdt
           FROM {}.funding
           WHERE symbol IS NOT NULL
             AND event_time_ms >= $1 AND event_time_ms <= $2
           ORDER BY event_time_ms,symbol,record_id"#,
        strategy.db_schema
    );
    let funding_events =
        sqlx::query_as::<_, IntraAnalysisFundingRecord>(sqlx::AssertSqlSafe(funding_sql))
            .bind(start_ms)
            .bind(end_ms)
            .fetch_all(&state.pool)
            .await?
            .into_iter()
            .map(|row| IntraAnalysisFundingEvent {
                symbol: row.symbol,
                ts: row.ts,
                amount_usdt: row.amount_usdt,
            })
            .collect::<Vec<_>>();
    let selected_symbols = query
        .symbols
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|symbol| !symbol.is_empty())
        .map(str::to_ascii_uppercase)
        .collect::<Vec<_>>();
    let display_name = strategy
        .alias
        .filter(|alias| !alias.trim().is_empty())
        .unwrap_or_else(|| strategy.slug.clone());
    let calculation = IntraAnalysisRequest {
        strategy_slug: strategy.slug,
        display_name,
        premium_adapter: adapter.premium_adapter,
        strategy_start_ms: strategy.st_ms,
        start_ms,
        end_ms,
        selected_symbols,
        reference_fee_bps,
        max_points: query.max_points.unwrap_or(3_000).clamp(200, 10_000),
        max_matches: query.max_matches.unwrap_or(200).clamp(20, 500),
    };
    let response = tokio::task::spawn_blocking(move || {
        intra_analysis::calculate_with_fees_and_funding(
            orders,
            fee_events,
            funding_events,
            calculation,
        )
    })
    .await
    .context("join intra combination FIFO calculation")??;
    Ok(Json(response).into_response())
}

fn intra_analysis_adapter(strategy: &IntraAnalysisStrategyRecord) -> Option<IntraAnalysisAdapter> {
    match (
        strategy.slug.as_str(),
        strategy.exchange.as_str(),
        strategy.strategy_kind.as_str(),
    ) {
        ("binance-intra-arb01", "binance", "intra_exchange") => Some(IntraAnalysisAdapter {
            premium_table: "binance_premium_index_klines",
            premium_adapter: "binance_premium_index_klines_1m",
        }),
        ("bybit-intra-arb01", "bybit", "intra_exchange") => Some(IntraAnalysisAdapter {
            premium_table: "bybit_premium_index_klines",
            premium_adapter: "bybit_premium_index_klines_1m",
        }),
        _ => None,
    }
}

fn expected_history_datasets(
    slug: &str,
    host: &str,
    exchange: &str,
    strategy_kind: &str,
) -> Option<&'static [&'static str]> {
    const BINANCE_FR: &[&str] = &["trades", "funding", "interest", "liquidations"];
    const GATE_FR: &[&str] = &["trades", "funding", "interest", "liquidations"];
    const BITGET_FR: &[&str] = &["trades", "funding", "interest"];
    const BINANCE_INTRA: &[&str] = &["trades", "funding"];
    const BYBIT_INTRA: &[&str] = &["trades", "funding", "interest"];
    const MM: &[&str] = &["trades"];

    let scheduled = (host == "local" && exchange == "binance" && strategy_kind == "funding_rate")
        || matches!(
            slug,
            "binance_mm_alpha"
                | "bybit_mm_alpha"
                | "binance-intra-arb01"
                | "bybit-intra-arb01"
                | "bybit-intra-arb02"
                | "bitget_fr_arb02"
                | "gate_fr_arb01"
                | "gate_fr_arb02"
        );
    if !scheduled {
        return None;
    }
    match (exchange, strategy_kind) {
        ("binance", "funding_rate") => Some(BINANCE_FR),
        ("gate", "funding_rate") => Some(GATE_FR),
        ("bitget", "funding_rate") => Some(BITGET_FR),
        ("binance", "intra_exchange") => Some(BINANCE_INTRA),
        ("bybit", "intra_exchange") => Some(BYBIT_INTRA),
        ("binance" | "bybit", "market_making") => Some(MM),
        _ => None,
    }
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

async fn list_strategy_snapshots(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<Vec<SnapshotSummaryResponse>>, ApiError> {
    let rows = sqlx::query_as::<_, SnapshotRecord>(
        r#"SELECT strategy_slug,snapshot_ts_ms,
                  (EXTRACT(EPOCH FROM fetched_at) * 1000)::bigint AS fetched_at_ms,
                  source_url,payload
           FROM strategy_snapshots
           WHERE strategy_slug=$1
           ORDER BY snapshot_ts_ms DESC"#,
    )
    .bind(slug)
    .fetch_all(&state.pool)
    .await?;
    Ok(Json(
        rows.into_iter().map(snapshot_summary_response).collect(),
    ))
}

async fn get_initial_snapshot(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> Result<Json<Option<SnapshotSummaryResponse>>, ApiError> {
    let row = load_initial_snapshot(&state.pool, &slug).await?;
    Ok(Json(row.map(snapshot_summary_response)))
}

async fn set_initial_snapshot(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
    Json(request): Json<SetInitialSnapshotRequest>,
) -> Result<Response, ApiError> {
    let strategy_start_ms =
        sqlx::query_scalar::<_, i64>("SELECT st_ms FROM strategy_envs WHERE enabled AND slug=$1")
            .bind(&slug)
            .fetch_optional(&state.pool)
            .await?;
    let Some(strategy_start_ms) = strategy_start_ms else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "strategy not found",
            }),
        )
            .into_response());
    };
    if request.snapshot_ts_ms < strategy_start_ms {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "initial snapshot must not be earlier than strategy stMs",
            }),
        )
            .into_response());
    }

    let snapshot = load_snapshot(&state.pool, &slug, request.snapshot_ts_ms).await?;
    let Some(snapshot) = snapshot else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "snapshot not found",
            }),
        )
            .into_response());
    };
    let parsed = parse_initial_positions(&snapshot.payload)?;
    if parsed.positions.is_empty() {
        return Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(ErrorResponse {
                error: "snapshot does not contain priced spot/swap positions",
            }),
        )
            .into_response());
    }

    sqlx::query(
        r#"INSERT INTO strategy_initial_snapshots
              (strategy_slug,snapshot_ts_ms,selected_at)
           VALUES ($1,$2,CURRENT_TIMESTAMP)
           ON CONFLICT (strategy_slug) DO UPDATE SET
              snapshot_ts_ms=EXCLUDED.snapshot_ts_ms,
              selected_at=CURRENT_TIMESTAMP"#,
    )
    .bind(&slug)
    .bind(request.snapshot_ts_ms)
    .execute(&state.pool)
    .await?;
    Ok(Json(snapshot_summary_response(snapshot)).into_response())
}

async fn clear_initial_snapshot(
    State(state): State<AppState>,
    AxumPath(slug): AxumPath<String>,
) -> Result<StatusCode, ApiError> {
    sqlx::query("DELETE FROM strategy_initial_snapshots WHERE strategy_slug=$1")
        .bind(slug)
        .execute(&state.pool)
        .await?;
    Ok(StatusCode::NO_CONTENT)
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

fn snapshot_summary_response(row: SnapshotRecord) -> SnapshotSummaryResponse {
    SnapshotSummaryResponse {
        strategy_slug: row.strategy_slug,
        snapshot_ts_ms: row.snapshot_ts_ms,
        fetched_at_ms: row.fetched_at_ms,
        source_url: row.source_url,
    }
}

async fn load_snapshot(
    pool: &PgPool,
    slug: &str,
    snapshot_ts_ms: i64,
) -> Result<Option<SnapshotRecord>> {
    sqlx::query_as::<_, SnapshotRecord>(
        r#"SELECT strategy_slug,snapshot_ts_ms,
                  (EXTRACT(EPOCH FROM fetched_at) * 1000)::bigint AS fetched_at_ms,
                  source_url,payload
           FROM strategy_snapshots
           WHERE strategy_slug=$1 AND snapshot_ts_ms=$2"#,
    )
    .bind(slug)
    .bind(snapshot_ts_ms)
    .fetch_optional(pool)
    .await
    .context("load strategy snapshot")
}

async fn load_initial_snapshot(pool: &PgPool, slug: &str) -> Result<Option<SnapshotRecord>> {
    sqlx::query_as::<_, SnapshotRecord>(
        r#"SELECT snapshots.strategy_slug,snapshots.snapshot_ts_ms,
                  (EXTRACT(EPOCH FROM snapshots.fetched_at) * 1000)::bigint AS fetched_at_ms,
                  snapshots.source_url,snapshots.payload
           FROM strategy_initial_snapshots selected
           JOIN strategy_snapshots snapshots
             ON snapshots.strategy_slug=selected.strategy_slug
            AND snapshots.snapshot_ts_ms=selected.snapshot_ts_ms
           WHERE selected.strategy_slug=$1"#,
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .context("load initial strategy snapshot")
}

fn parse_initial_positions(payload: &Value) -> Result<ParsedInitialPositions> {
    let entries = payload
        .get("entries")
        .and_then(Value::as_array)
        .context("snapshot entries are missing")?;
    let rows = entries
        .iter()
        .find(|entry| entry.get("type").and_then(Value::as_str) == Some("pre_trade_exposure"))
        .and_then(|entry| entry.get("entry"))
        .and_then(|entry| entry.get("rows"))
        .and_then(Value::as_array)
        .context("snapshot pre_trade_exposure rows are missing")?;

    let mut positions = Vec::new();
    let mut skipped_count = 0;
    for row in rows {
        if row.get("is_total").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(asset) = row.get("asset").and_then(Value::as_str) else {
            continue;
        };
        let spot_quantity = row.get("open_qty").and_then(Value::as_f64).unwrap_or(0.0);
        let futures_quantity = row.get("hedge_qty").and_then(Value::as_f64).unwrap_or(0.0);
        if !spot_quantity.is_finite() || !futures_quantity.is_finite() {
            bail!("snapshot contains a non-finite position quantity");
        }
        if spot_quantity.abs() <= f64::EPSILON && futures_quantity.abs() <= f64::EPSILON {
            continue;
        }

        let mut priced_quantity = 0.0;
        let mut priced_notional = 0.0;
        for (quantity, field) in [
            (spot_quantity, "open_usdt"),
            (futures_quantity, "hedge_usdt"),
        ] {
            let notional = row.get(field).and_then(Value::as_f64).unwrap_or(0.0);
            if quantity.abs() > f64::EPSILON && notional.is_finite() && notional.abs() > 0.0 {
                priced_quantity += quantity.abs();
                priced_notional += notional.abs();
            }
        }
        if priced_quantity <= f64::EPSILON || priced_notional <= f64::EPSILON {
            skipped_count += 1;
            continue;
        }
        let asset = asset.trim().to_ascii_uppercase();
        let symbol = if asset.ends_with("USDT") {
            asset
        } else {
            format!("{asset}USDT")
        };
        positions.push(InitialPosition {
            symbol,
            spot_quantity,
            futures_quantity,
            mark_price: priced_notional / priced_quantity,
        });
    }
    Ok(ParsedInitialPositions {
        positions,
        skipped_count,
    })
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
        read_only: state.read_only,
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
    .bind(&slug)
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

    let initial_snapshot = load_initial_snapshot(&state.pool, &slug).await?;
    let parsed_initial = initial_snapshot
        .as_ref()
        .map(|snapshot| parse_initial_positions(&snapshot.payload))
        .transpose()?;
    let initial_snapshot_ts_ms = initial_snapshot
        .as_ref()
        .map(|snapshot| snapshot.snapshot_ts_ms);
    let effective_start_ms = initial_snapshot_ts_ms.unwrap_or(strategy.st_ms);
    let start_ms = query.start_ms.unwrap_or(effective_start_ms);
    let end_ms = query
        .end_ms
        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
    if start_ms < effective_start_ms {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "startMs must not be earlier than the effective PnL start",
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
    let load_start_ms = initial_snapshot_ts_ms
        .map(|snapshot_ts_ms| snapshot_ts_ms.saturating_add(1))
        .unwrap_or(strategy.st_ms);
    let mut inputs = pnl::load_inputs(
        &state.pool,
        source,
        &strategy.db_schema,
        &strategy.exchange,
        &state.mark_prices,
        load_start_ms,
        end_ms,
    )
    .await?;
    if let Some(parsed) = parsed_initial.as_ref() {
        inputs.initial_positions = parsed.positions.clone();
    }
    let response = pnl::calculate(
        inputs,
        PnlCalculation {
            source,
            exchange: strategy.exchange,
            strategy_start_ms: effective_start_ms,
            start_ms,
            end_ms,
            selected_symbols,
            max_points: query.max_points.unwrap_or(3_000).clamp(200, 10_000),
            initial_snapshot_ts_ms,
            skipped_initial_position_count: parsed_initial
                .as_ref()
                .map(|parsed| parsed.skipped_count)
                .unwrap_or_default(),
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
        IntraAnalysisStrategyRecord, assigned_env_keys, expected_history_datasets,
        intra_analysis_adapter, is_default_bybit_instrument, parse_initial_positions,
        snapshot_source_url, valid_schema,
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
    fn exposes_fifo_analysis_only_for_intra_arb01_strategies() {
        let strategy = |slug: &str, exchange: &str| IntraAnalysisStrategyRecord {
            slug: slug.to_string(),
            alias: None,
            db_schema: slug.replace('-', "_"),
            st_ms: 1,
            exchange: exchange.to_string(),
            strategy_kind: "intra_exchange".to_string(),
        };

        let binance = strategy("binance-intra-arb01", "binance");
        let bybit = strategy("bybit-intra-arb01", "bybit");
        let bybit_arb02 = strategy("bybit-intra-arb02", "bybit");

        assert_eq!(
            intra_analysis_adapter(&binance)
                .expect("Binance arb01 must support FIFO analysis")
                .premium_table,
            "binance_premium_index_klines"
        );
        assert_eq!(
            intra_analysis_adapter(&bybit)
                .expect("Bybit arb01 must support FIFO analysis")
                .premium_table,
            "bybit_premium_index_klines"
        );
        assert!(intra_analysis_adapter(&bybit_arb02).is_none());
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

    #[test]
    fn parses_priced_initial_positions_and_reports_unpriced_balances() {
        let payload = serde_json::json!({
            "entries": [{
                "type": "pre_trade_exposure",
                "entry": {"rows": [
                    {
                        "asset": "BTC",
                        "is_total": false,
                        "open_qty": -2.0,
                        "hedge_qty": 1.0,
                        "open_usdt": -200.0,
                        "hedge_usdt": 100.0
                    },
                    {
                        "asset": "EUR",
                        "is_total": false,
                        "open_qty": 0.01,
                        "hedge_qty": 0.0,
                        "open_usdt": 0.0,
                        "hedge_usdt": 0.0
                    },
                    {
                        "asset": "TOTAL",
                        "is_total": true
                    }
                ]}
            }]
        });

        let parsed = parse_initial_positions(&payload).unwrap();

        assert_eq!(parsed.positions.len(), 1);
        assert_eq!(parsed.skipped_count, 1);
        assert_eq!(parsed.positions[0].symbol, "BTCUSDT");
        assert_eq!(parsed.positions[0].spot_quantity, -2.0);
        assert_eq!(parsed.positions[0].futures_quantity, 1.0);
        assert_eq!(parsed.positions[0].mark_price, 100.0);
    }

    #[test]
    fn identifies_recurring_history_datasets() {
        assert_eq!(
            expected_history_datasets("binance_fr_arb01", "local", "binance", "funding_rate"),
            Some(["trades", "funding", "interest", "liquidations"].as_slice())
        );
        assert_eq!(
            expected_history_datasets("bybit-intra-arb01", "sg", "bybit", "intra_exchange"),
            Some(["trades", "funding", "interest"].as_slice())
        );
        assert_eq!(
            expected_history_datasets("bybit_mm_alpha", "sg", "bybit", "market_making"),
            Some(["trades"].as_slice())
        );
    }
}
