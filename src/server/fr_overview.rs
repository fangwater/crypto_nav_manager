//! Read-only FR account summaries for a polling Manager. No credentials, paths,
//! raw fills or trading controls cross this boundary.
use super::*;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::RwLock, time::Duration};

const DAY: i64 = 86_400_000;
const FRESH: i64 = 90_000;
const TOKEN_ENV: &str = "CRYPTO_NAV_OVERVIEW_TOKEN";
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn day(ts: i64) -> i64 {
    (ts + 8 * 3_600_000).div_euclid(DAY) * DAY - 8 * 3_600_000
}
fn label(ts: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts + 8 * 3_600_000)
        .unwrap()
        .format("%Y-%m-%d")
        .to_string()
}

#[derive(Clone, Default, Serialize)]
pub(super) struct Row {
    source_id: String,
    account: String,
    venue: String,
    yesterday_equity: Option<f64>,
    equity: Option<f64>,
    today_profit: Option<f64>,
    profit_percent: Option<f64>,
    long_position: Option<f64>,
    short_position: Option<f64>,
    long_leverage: Option<f64>,
    short_leverage: Option<f64>,
    net_position: Option<f64>,
    max_drawdown_15d_percent: Option<f64>,
    margin: Option<f64>,
    margin_percent: Option<f64>,
    date_ms: Option<i64>,
    queried_at_ms: i64,
    history_start_ms: Option<i64>,
    history_rebuilt_at_ms: Option<i64>,
    collection_status: String,
}

#[derive(Clone)]
struct Reconstruction {
    at: i64,
    equity: f64,
    start: i64,
    history: pnl::overview::History,
}
impl Reconstruction {
    fn equity_at(&self, ts: i64) -> Option<f64> {
        self.history
            .boundaries
            .get(&ts)
            .map(|p| self.equity - self.history.final_pnl + p)
    }
    fn daily_realized_profit(&self, start: i64, end: Option<i64>) -> Option<f64> {
        let opening = *self.history.realized_boundaries.get(&start)?;
        let closing = match end {
            Some(end) => *self.history.realized_boundaries.get(&end)?,
            None if day(self.at) == start => self.history.final_realized_net,
            None => return None,
        };
        Some(closing - opening)
    }
    fn drawdown(&self, end: i64) -> Option<f64> {
        let start = end - 15 * DAY;
        if start < self.start {
            return None;
        }
        let start_pnl = self
            .history
            .boundaries
            .get(&start)
            .copied()
            .unwrap_or_else(|| {
                self.history
                    .points
                    .iter()
                    .rev()
                    .find(|(ts, _)| *ts < start)
                    .map_or(self.history.start_pnl, |(_, p)| *p)
            });
        let mut peak = self.equity - self.history.final_pnl + start_pnl;
        if peak <= 0.0 {
            return None;
        }
        let mut maximum = 0.0f64;
        for (_, pnl) in self
            .history
            .points
            .iter()
            .filter(|(ts, _)| *ts >= start && *ts <= end)
        {
            let equity = self.equity - self.history.final_pnl + pnl;
            peak = peak.max(equity);
            maximum = maximum.max((peak - equity) / peak * 100.0);
        }
        Some(maximum)
    }
}

#[derive(Clone)]
struct Account {
    row: Row,
    reconstruction: Option<Arc<Reconstruction>>,
    exposure_at: Option<i64>,
    history_attempt_at: i64,
}
#[derive(Default)]
struct Cache {
    accounts: BTreeMap<String, Account>,
    loaded: bool,
}
#[derive(Clone)]
pub(super) struct FrOverview {
    token_hash: Option<[u8; 32]>,
    cache: Arc<RwLock<Cache>>,
    client: reqwest::Client,
}
impl FrOverview {
    pub(super) fn new() -> Result<Self> {
        let token = env::var(TOKEN_ENV).ok();
        if let Some(token) = &token {
            anyhow::ensure!(
                (32..=256).contains(&token.len()),
                "{TOKEN_ENV} must have 32-256 characters"
            );
        }
        Ok(Self {
            token_hash: token.map(|v| Sha256::digest(v.as_bytes()).into()),
            cache: Arc::default(),
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(8))
                .build()?,
        })
    }
    fn authorized(&self, headers: &HeaderMap) -> bool {
        let Some(expected) = self.token_hash else {
            return false;
        };
        let Some(token) = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
        else {
            return false;
        };
        let hash = Sha256::digest(token.as_bytes());
        expected.iter().zip(hash).fold(0u8, |v, (a, b)| v | (a ^ b)) == 0
    }
    pub(super) fn spawn(&self, pool: PgPool, risks: AccountRiskCache, marks: MarkPriceCache) {
        if self.token_hash.is_none() {
            return;
        }
        let hub = self.clone();
        tokio::spawn(async move {
            loop {
                if let Err(error) = hub.refresh(&pool, &risks, &marks).await {
                    warn!(%error,"FR overview refresh failed");
                }
                tokio::time::sleep(Duration::from_secs(15)).await;
            }
        });
    }
    async fn refresh(
        &self,
        pool: &PgPool,
        risks: &AccountRiskCache,
        marks: &MarkPriceCache,
    ) -> Result<()> {
        let strategies=sqlx::query_as::<_,StrategyRecord>("SELECT slug,alias,db_schema,host,env_path,csv_output_dir,st_ms,strategy_kind,exchange,account_mode,required_keys,config_url,sort_order FROM strategy_envs WHERE enabled AND strategy_kind='funding_rate' ORDER BY sort_order,slug").fetch_all(pool).await?;
        let slugs = strategies
            .iter()
            .map(|s| s.slug.clone())
            .collect::<HashSet<_>>();
        let snapshots = risks
            .snapshots()
            .into_iter()
            .map(|r| (r.strategy_slug.clone(), r))
            .collect::<HashMap<_, _>>();
        // A single replay at a time bounds CPU/memory and leaves the normal API
        // responsive. Per-account publication lets other rows remain usable.
        for strategy in strategies {
            let old = self
                .cache
                .read()
                .unwrap()
                .accounts
                .get(&strategy.slug)
                .cloned();
            let mut account = old.unwrap_or_else(|| Account {
                row: Row {
                    source_id: strategy.slug.clone(),
                    ..Default::default()
                },
                reconstruction: None,
                exposure_at: None,
                history_attempt_at: 0,
            });
            account.row.account = strategy
                .alias
                .clone()
                .unwrap_or_else(|| strategy.slug.clone());
            account.row.venue = strategy.exchange.clone();
            let time = now();
            account.row.queried_at_ms = time;
            let mut risk = snapshots
                .get(&strategy.slug)
                .filter(|r| r.status == "live")
                .and_then(|r| {
                    Some(RiskFact {
                        ts: r.source_ts_ms?,
                        equity: r.actual_equity_usd?,
                        margin: r.initial_margin_usd,
                    })
                })
                .filter(|r| r.valid(time));
            let url = sqlx::query_scalar::<_, Option<String>>(
                "SELECT snapshot_url FROM strategy_envs WHERE slug=$1",
            )
            .bind(&strategy.slug)
            .fetch_one(pool)
            .await?
            .map(Ok)
            .unwrap_or_else(|| snapshot_source_url(&strategy.host, &strategy.config_url));
            let exposure = async {
                let payload: Value = self
                    .client
                    .get(url?)
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                Ok::<_, anyhow::Error>((
                    exposure(&payload),
                    snapshot_risk(&payload).filter(|r| r.valid(time)),
                ))
            }
            .await;
            if let Ok((exposure, snapshot_risk)) = exposure {
                match exposure {
                    Ok((ts, long, short)) => {
                        account.row.long_position = Some(long);
                        account.row.short_position = Some(short);
                        account.exposure_at = Some(ts);
                    }
                    Err(_) => {
                        account.row.long_position = None;
                        account.row.short_position = None;
                        account.exposure_at = None;
                    }
                }
                if snapshot_risk
                    .as_ref()
                    .is_some_and(|r| risk.as_ref().is_none_or(|old| r.ts > old.ts))
                {
                    risk = snapshot_risk;
                }
            }
            if let Some(risk) = &risk {
                account.row.equity = Some(risk.equity);
                account.row.date_ms = Some(risk.ts);
                account.row.margin = risk.margin;
            }
            let live = risk.is_some()
                && account
                    .row
                    .date_ms
                    .is_some_and(|ts| (0..=FRESH).contains(&(time - ts)));
            account.row.collection_status = if live {
                "live"
            } else if account.row.equity.is_some() {
                "stale"
            } else {
                "unavailable"
            }
            .into();
            if live
                && time - account.history_attempt_at >= 300_000
                && account
                    .reconstruction
                    .as_ref()
                    .is_none_or(|r| time - r.at >= 300_000 || day(r.at) != day(time))
            {
                account.history_attempt_at = time;
                match reconstruct(
                    pool,
                    marks,
                    &strategy,
                    account.row.equity.unwrap(),
                    account.row.date_ms.unwrap(),
                )
                .await
                {
                    Ok(reconstruction) => account.reconstruction = Some(Arc::new(reconstruction)),
                    Err(error) => {
                        warn!(slug=%strategy.slug,%error,"FR overview history unavailable")
                    }
                }
            }
            self.cache
                .write()
                .unwrap()
                .accounts
                .insert(strategy.slug, account);
        }
        let mut cache = self.cache.write().unwrap();
        cache.accounts.retain(|slug, _| slugs.contains(slug));
        cache.loaded = true;
        Ok(())
    }
}

fn exposure(payload: &Value) -> Result<(i64, f64, f64)> {
    let entry = payload["entries"]
        .as_array()
        .context("missing snapshot entries")?
        .iter()
        .find(|e| e["type"] == "pre_trade_exposure")
        .context("missing factual exposure")?;
    let entry = &entry["entry"];
    let ts = entry["ts_ms"].as_i64().context("missing exposure time")?;
    let rows = entry["rows"].as_array().context("missing exposure rows")?;
    let (mut long, mut short) = (0.0, 0.0);
    for row in rows.iter().filter(|r| r["is_total"] != true) {
        for (qty, notional) in [("open_qty", "open_usdt"), ("hedge_qty", "hedge_usdt")] {
            let qty = row[qty].as_f64().context("missing factual quantity")?;
            let value = row[notional].as_f64().context("missing factual notional")?;
            anyhow::ensure!(
                value.is_finite() && qty.is_finite() && (qty == 0.0 || value != 0.0),
                "invalid exposure valuation"
            );
            if qty > 0.0 {
                long += value.abs();
            } else if qty < 0.0 {
                short += value.abs();
            }
        }
    }
    Ok((ts, long, short))
}

struct RiskFact {
    ts: i64,
    equity: f64,
    margin: Option<f64>,
}
impl RiskFact {
    fn valid(&self, time: i64) -> bool {
        (0..=FRESH).contains(&(time - self.ts))
            && self.equity.is_finite()
            && self.margin.is_none_or(|v| v.is_finite() && v >= 0.0)
    }
}
// Viz carries the same account-monitor facts as IPC. Use their original time,
// never the enclosing page timestamp or a recomputed/adjusted-equity proxy.
fn snapshot_risk(payload: &Value) -> Option<RiskFact> {
    let entry = payload["entries"]
        .as_array()?
        .iter()
        .find(|e| e["type"] == "pre_trade_risk")?;
    let rows = entry["entry"]["account_risks"].as_array()?;
    if rows.len() != 1 {
        return None;
    }
    Some(RiskFact {
        ts: rows[0]["ts_ms"].as_i64()?,
        equity: rows[0]["actual_equity_usd"].as_f64()?,
        margin: rows[0]["initial_margin_usd"].as_f64(),
    })
}

async fn reconstruct(
    pool: &PgPool,
    marks: &MarkPriceCache,
    strategy: &StrategyRecord,
    equity: f64,
    at: i64,
) -> Result<Reconstruction> {
    let initial = load_initial_snapshot(pool, &strategy.slug).await?;
    let start = initial
        .as_ref()
        .map_or(strategy.st_ms, |s| s.snapshot_ts_ms);
    let mut inputs = pnl::load_inputs(
        pool,
        PnlSourceKind::FundingRate,
        &strategy.db_schema,
        &strategy.exchange,
        marks,
        if initial.is_some() { start + 1 } else { start },
        at,
    )
    .await?;
    if let Some(initial) = initial {
        let parsed = parse_initial_positions(&initial.payload)?;
        anyhow::ensure!(
            parsed.skipped_count == 0,
            "initial positions have missing prices"
        );
        inputs.initial_positions = parsed.positions;
    }
    let boundaries = (0..=46)
        .map(|i| day(at) - i * DAY)
        .filter(|ts| *ts >= start)
        .collect::<Vec<_>>();
    let history = tokio::task::spawn_blocking(move || {
        pnl::overview::replay(inputs, at - 46 * DAY, at, &boundaries)
    })
    .await??;
    Ok(Reconstruction {
        at,
        equity,
        start,
        history,
    })
}

#[derive(Deserialize)]
pub(super) struct QueryParams {
    date: Option<String>,
}
pub(super) async fn report(
    State(state): State<AppState>,
    Query(query): Query<QueryParams>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let hub = &state.fr_overview;
    if !hub.authorized(&headers) {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "integration token required",
            }),
        )
            .into_response());
    }
    let time = now();
    let selected = if let Some(date) = query.date {
        let parsed = chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
            .ok()
            .filter(|d| d.format("%Y-%m-%d").to_string() == date);
        let Some(parsed) = parsed else {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "invalid date",
                }),
            )
                .into_response());
        };
        let ts = parsed
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
            - 8 * 3_600_000;
        if ts > day(time) {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "future date is unavailable",
                }),
            )
                .into_response());
        }
        Some(ts)
    } else {
        None
    };
    let cache = hub.cache.read().unwrap();
    let mut accounts = Vec::new();
    let mut dates = std::collections::BTreeSet::new();
    for account in cache.accounts.values() {
        let mut row = account.row.clone();
        if let Some(r) = &account.reconstruction {
            for ts in r
                .history
                .boundaries
                .keys()
                .filter(|ts| **ts >= day(time) - 30 * DAY && **ts < day(time))
            {
                if r.equity_at(ts + DAY).is_some() {
                    dates.insert(label(*ts));
                }
            }
            let end = selected.filter(|ts| *ts < day(time)).map(|ts| ts + DAY);
            if let Some(end) = end {
                row.equity = r.equity_at(end);
                row.yesterday_equity = r.equity_at(end - DAY);
                row.date_ms = Some(end);
                row.long_position = None;
                row.short_position = None;
                row.margin = None;
                row.collection_status = if row.equity.is_some() {
                    "historical"
                } else {
                    "unavailable"
                }
                .into();
            } else {
                row.yesterday_equity = r.equity_at(day(time));
            }
            row.today_profit = r.daily_realized_profit(end.map_or(day(time), |end| end - DAY), end);
            row.max_drawdown_15d_percent = r.drawdown(end.unwrap_or(r.at));
            row.history_start_ms = Some(r.start);
            row.history_rebuilt_at_ms = Some(r.at);
        } else if selected.is_some_and(|ts| ts < day(time)) {
            row = Row {
                source_id: row.source_id,
                account: row.account,
                venue: row.venue,
                collection_status: "unavailable".into(),
                ..Default::default()
            };
        }
        if selected.is_none_or(|ts| ts == day(time))
            && (row
                .date_ms
                .is_none_or(|ts| !(0..=FRESH).contains(&(time - ts))))
        {
            row.collection_status = if row.equity.is_some() {
                "stale"
            } else {
                "unavailable"
            }
            .into();
        }
        if selected.is_none_or(|ts| ts == day(time))
            && account
                .exposure_at
                .is_none_or(|ts| !(0..=FRESH).contains(&(time - ts)))
        {
            row.long_position = None;
            row.short_position = None;
        }
        // Profit comes only from realized events; live equity movements must
        // never introduce floating PnL or deposits into the daily profit.

        row.profit_percent = ratio(row.today_profit, row.yesterday_equity).map(|r| r * 100.0);
        row.long_leverage = ratio(row.long_position, row.equity);
        row.short_leverage = ratio(row.short_position, row.equity);
        row.net_position = row
            .long_position
            .zip(row.short_position)
            .map(|(l, s)| l - s);
        row.margin_percent = ratio(row.margin, row.equity).map(|r| r * 100.0);
        accounts.push(row);
    }
    Ok(([(header::CACHE_CONTROL,"no-store")],Json(serde_json::json!({"accounts":accounts,"dates":dates.into_iter().rev().collect::<Vec<_>>(),"generated_at_ms":time,"loading":!cache.loaded}))).into_response())
}
fn ratio(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    a.zip(b).filter(|(_, b)| *b > 0.0).map(|(a, b)| a / b)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn viz_risk_requires_factual_equity_and_its_own_fresh_timestamp() {
        let mut payload = serde_json::json!({"entries":[{"type":"pre_trade_risk","entry":{"ts_ms":1000000,"total_equity":100,"account_risks":[{"ts_ms":100,"actual_equity_usd":90,"initial_margin_usd":10}]}}]});
        let risk = snapshot_risk(&payload).unwrap();
        assert_eq!(risk.equity, 90.0);
        assert!(risk.valid(101));
        assert!(!risk.valid(1000000));
        payload["entries"][0]["entry"]["account_risks"][0]
            .as_object_mut()
            .unwrap()
            .remove("actual_equity_usd");
        assert!(snapshot_risk(&payload).is_none());
    }
    #[test]
    fn reconstructed_equity_drawdown_uses_complete_history_and_never_fills_missing_days() {
        let reconstruction = Reconstruction {
            at: 20 * DAY,
            equity: 100.0,
            start: 0,
            history: pnl::overview::History {
                boundaries: BTreeMap::from([(5 * DAY, 10.0), (20 * DAY, 0.0)]),
                points: vec![(6 * DAY, 20.0), (7 * DAY, -10.0), (20 * DAY, 0.0)],
                realized_boundaries: BTreeMap::from([(5 * DAY, 3.0), (20 * DAY, 8.0)]),
                final_realized_net: 10.0,
                final_pnl: 0.0,
                start_pnl: 0.0,
            },
        };
        assert_eq!(reconstruction.equity_at(5 * DAY), Some(110.0));
        assert_eq!(reconstruction.equity_at(19 * DAY), None);
        assert_eq!(reconstruction.drawdown(20 * DAY), Some(25.0));
        assert_eq!(reconstruction.drawdown(14 * DAY), None);
        assert_eq!(
            reconstruction.daily_realized_profit(5 * DAY, Some(20 * DAY)),
            Some(5.0)
        );
        let mut changed_equity = reconstruction.clone();
        let opening = day(changed_equity.at);
        changed_equity
            .history
            .realized_boundaries
            .insert(opening, 8.0);
        assert_eq!(
            changed_equity.daily_realized_profit(opening, None),
            Some(2.0)
        );
        assert_eq!(
            changed_equity.daily_realized_profit(opening - DAY, None),
            None
        );
        changed_equity.equity = 2000.0;
        assert_eq!(
            changed_equity.daily_realized_profit(opening, None),
            Some(2.0)
        );
    }
    #[test]
    fn exposure_keeps_both_legs_and_skips_total_rows() {
        let mut payload = serde_json::json!({"entries":[{"type":"pre_trade_exposure","entry":{"ts_ms":100,"rows":[{"open_qty":2,"open_usdt":20,"hedge_qty":-3,"hedge_usdt":-30},{"is_total":true,"open_qty":2,"open_usdt":20,"hedge_qty":-3,"hedge_usdt":-30}]}}]});
        assert_eq!(exposure(&payload).unwrap(), (100, 20.0, 30.0));
        assert!(exposure(&serde_json::json!({"entries":[]})).is_err());
        payload["entries"][0]["entry"]["rows"][0]["open_usdt"] = serde_json::json!(0);
        assert!(
            exposure(&payload).is_err(),
            "an unpriced holding must not become zero exposure"
        );
    }
    #[test]
    fn integration_token_does_not_accept_sessions_or_missing_tokens() {
        let mut hub = FrOverview::new().unwrap();
        hub.token_hash = Some(Sha256::digest(b"test-secret").into());
        let mut headers = HeaderMap::new();
        assert!(!hub.authorized(&headers));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer other"),
        );
        assert!(!hub.authorized(&headers));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer test-secret"),
        );
        assert!(hub.authorized(&headers));
        hub.token_hash = None;
        assert!(!hub.authorized(&headers));
    }
}
