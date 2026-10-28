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
    account_kind: String,
    yesterday_equity: Option<f64>,
    equity: Option<f64>,
    today_profit: Option<f64>,
    profit_percent: Option<f64>,
    long_position: Option<f64>,
    short_position: Option<f64>,
    long_leverage: Option<f64>,
    short_leverage: Option<f64>,
    net_position: Option<f64>,
    position_unpriced_assets: Vec<String>,
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
    equity: Option<f64>,
    start: i64,
    history: pnl::overview::History,
}
impl Reconstruction {
    fn equity_at(&self, ts: i64) -> Option<f64> {
        let equity = self.equity?;
        self.history
            .boundaries
            .get(&ts)
            .map(|p| equity - self.history.final_pnl + p)
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
        let anchor_equity = self.equity?;
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
        let mut peak = anchor_equity - self.history.final_pnl + start_pnl;
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
            let equity = anchor_equity - self.history.final_pnl + pnl;
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
    monitor_reconstruction: Option<Arc<Reconstruction>>,
    exposure_at: Option<i64>,
    history_attempt_at: i64,
    history_error: Option<String>,
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
    history_gate: Arc<tokio::sync::Semaphore>,
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
            history_gate: Arc::new(tokio::sync::Semaphore::new(1)),
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
        let strategies=sqlx::query_as::<_,StrategyRecord>("SELECT slug,alias,db_schema,host,env_path,csv_output_dir,st_ms,strategy_kind,exchange,account_mode,required_keys,config_url,sort_order FROM strategy_envs WHERE enabled AND (strategy_kind='funding_rate' OR (strategy_kind='market_making' AND slug IN ('okex_mm_alpha','bybit_mm_alpha'))) ORDER BY sort_order,slug").fetch_all(pool).await?;
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
                monitor_reconstruction: None,
                exposure_at: None,
                history_attempt_at: 0,
                history_error: None,
            });
            account.row.account = strategy
                .alias
                .clone()
                .unwrap_or_else(|| strategy.slug.clone());
            account.row.venue = strategy.exchange.clone();
            account.row.account_kind = if strategy.strategy_kind == "market_making" {
                "mm"
            } else {
                "fr"
            }
            .into();
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
                    Ok(exposure) => {
                        account.row.long_position = exposure.long;
                        account.row.short_position = exposure.short;
                        account.row.position_unpriced_assets = exposure.unpriced_assets;
                        account.exposure_at = Some(exposure.ts);
                    }
                    Err(_) => {
                        account.row.long_position = None;
                        account.row.short_position = None;
                        account.row.position_unpriced_assets.clear();
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
            let history_permit = if time - account.history_attempt_at >= 300_000
                && account
                    .monitor_reconstruction
                    .as_ref()
                    .is_none_or(|r| time - r.at >= 300_000 || day(r.at) != day(time))
            {
                self.history_gate.clone().try_acquire_owned().ok()
            } else {
                None
            };
            if history_permit.is_some() {
                account.history_attempt_at = time;
            }
            // PnL monitoring needs no live-equity anchor. Daily reconstructed
            // equity remains unknown when a fresh factual anchor is absent.
            let anchor = if live {
                (account.row.equity, account.row.date_ms.unwrap())
            } else {
                (None, time)
            };
            self.cache
                .write()
                .unwrap()
                .accounts
                .insert(strategy.slug.clone(), account);
            if let Some(permit) = history_permit {
                let hub = self.clone();
                let pool = pool.clone();
                let marks = marks.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let (equity, at) = anchor;
                    match reconstruct(&pool, &marks, &strategy, equity, at).await {
                        Ok(value) => {
                            if let Some(account) =
                                hub.cache.write().unwrap().accounts.get_mut(&strategy.slug)
                            {
                                let value = Arc::new(value);
                                if value.equity.is_some() {
                                    account.reconstruction = Some(value.clone());
                                }
                                account.monitor_reconstruction = Some(value);
                                account.history_error = None;
                            }
                        }
                        Err(error) => {
                            if let Some(account) =
                                hub.cache.write().unwrap().accounts.get_mut(&strategy.slug)
                            {
                                account.history_error =
                                    Some("NAV 历史重算暂不可用，后台会重试".into());
                            }
                            warn!(slug=%strategy.slug,%error,"account overview history unavailable")
                        }
                    }
                });
            }
        }
        let mut cache = self.cache.write().unwrap();
        cache.accounts.retain(|slug, _| slugs.contains(slug));
        cache.loaded = true;
        Ok(())
    }
}

#[derive(Debug, PartialEq)]
struct Exposure {
    ts: i64,
    long: Option<f64>,
    short: Option<f64>,
    unpriced_assets: Vec<String>,
}

fn exposure(payload: &Value) -> Result<Exposure> {
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
    let (mut unpriced_long, mut unpriced_short) = (false, false);
    let mut unpriced_assets = std::collections::BTreeSet::new();
    for row in rows.iter().filter(|r| r["is_total"] != true) {
        for (qty, notional) in [("open_qty", "open_usdt"), ("hedge_qty", "hedge_usdt")] {
            let qty = row[qty].as_f64().context("missing factual quantity")?;
            let value = row[notional].as_f64().context("missing factual notional")?;
            anyhow::ensure!(
                value.is_finite() && qty.is_finite() && (qty != 0.0 || value == 0.0),
                "invalid exposure valuation"
            );
            if qty != 0.0 && value == 0.0 {
                // Preserve measured legs, but never claim that an unpriced
                // holding is flat or that its unknown notional is dust.
                let asset = row["asset"].as_str().context("missing unpriced asset")?;
                anyhow::ensure!(!asset.trim().is_empty(), "empty unpriced asset");
                unpriced_assets.insert(asset.to_string());
                unpriced_long |= qty > 0.0;
                unpriced_short |= qty < 0.0;
                continue;
            }
            if qty > 0.0 {
                long += value.abs();
            } else if qty < 0.0 {
                short += value.abs();
            }
        }
    }
    anyhow::ensure!(long.is_finite() && short.is_finite(), "exposure overflow");
    Ok(Exposure {
        ts,
        long: (long > 0.0 || !unpriced_long).then_some(long),
        short: (short > 0.0 || !unpriced_short).then_some(short),
        unpriced_assets: unpriced_assets.into_iter().collect(),
    })
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
    equity: Option<f64>,
    at: i64,
) -> Result<Reconstruction> {
    let source = PnlSourceKind::for_strategy(
        &strategy.strategy_kind,
        &strategy.exchange,
        &strategy.account_mode,
    )
    .context("unsupported overview PnL source")?;
    let initial = load_initial_snapshot(pool, &strategy.slug).await?;
    let start = initial
        .as_ref()
        .map_or(strategy.st_ms, |s| s.snapshot_ts_ms);
    let mut inputs = pnl::load_inputs(
        pool,
        source,
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
    let mut boundaries = (0..=46)
        .map(|i| day(at) - i * DAY)
        .filter(|ts| *ts >= start)
        .collect::<Vec<_>>();
    boundaries.extend(monitor_boundaries(start, at)?);
    let history = tokio::task::spawn_blocking(move || {
        pnl::overview::replay(source, inputs, at - 46 * DAY, at, &boundaries)
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
            for ts in
                r.history.boundaries.keys().filter(|ts| {
                    day(**ts) == **ts && **ts >= day(time) - 30 * DAY && **ts < day(time)
                })
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
                row.position_unpriced_assets.clear();
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
                account_kind: row.account_kind,
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
            row.position_unpriced_assets.clear();
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
    fn monitoring_is_zero_based_and_fr_excludes_floating_without_equity() {
        let hour = 3_600_000;
        let start = hour;
        let end = 10 * hour;
        assert_eq!(
            monitor_boundaries(start, end).unwrap(),
            vec![hour, 4 * hour, 8 * hour, 10 * hour]
        );
        let r = Arc::new(Reconstruction {
            at: end,
            equity: None,
            start,
            history: pnl::overview::History {
                boundaries: BTreeMap::from([
                    (start, 0.0),
                    (4 * hour, 100.0),
                    (8 * hour, 200.0),
                    (end, 300.0),
                ]),
                realized_boundaries: BTreeMap::from([
                    (start, 0.0),
                    (4 * hour, 1.0),
                    (8 * hour, -2.0),
                    (end, 3.0),
                ]),
                points: vec![],
                final_pnl: 300.0,
                final_realized_net: 3.0,
                start_pnl: 0.0,
            },
        });
        assert_eq!(r.equity_at(end), None);
        let mut account = Account {
            row: Row {
                source_id: "fr_a".into(),
                account_kind: "fr".into(),
                ..Default::default()
            },
            reconstruction: None,
            monitor_reconstruction: Some(r),
            exposure_at: None,
            history_attempt_at: 0,
            history_error: None,
        };
        let value = monitor_value(&account, end);
        assert_eq!(value["points"][0]["pnl_quote"], 0.0);
        assert_eq!(value["points"][3]["pnl_quote"], 3.0);
        assert_eq!(value["complete"], true);
        account.row.account_kind = "mm".into();
        assert_eq!(
            monitor_value(&account, end)["points"][3]["pnl_quote"],
            300.0
        );
        assert_eq!(monitor_value(&account, end + 600_001)["complete"], false);
    }
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
            equity: Some(100.0),
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
        changed_equity.equity = Some(2000.0);
        assert_eq!(
            changed_equity.daily_realized_profit(opening, None),
            Some(2.0)
        );
    }
    #[test]
    fn exposure_keeps_both_legs_and_skips_total_rows() {
        let mut payload = serde_json::json!({"entries":[{"type":"pre_trade_exposure","entry":{"ts_ms":100,"rows":[{"open_qty":2,"open_usdt":20,"hedge_qty":-3,"hedge_usdt":-30},{"is_total":true,"open_qty":2,"open_usdt":20,"hedge_qty":-3,"hedge_usdt":-30}]}}]});
        assert_eq!(
            exposure(&payload).unwrap(),
            Exposure {
                ts: 100,
                long: Some(20.0),
                short: Some(30.0),
                unpriced_assets: vec![],
            }
        );
        assert!(exposure(&serde_json::json!({"entries":[]})).is_err());
        payload["entries"][0]["entry"]["rows"][0]["open_usdt"] = serde_json::json!(0);
        assert!(
            exposure(&payload).is_err(),
            "an unpriced holding must identify the omitted asset"
        );
    }
    #[test]
    fn unpriced_holdings_preserve_priced_legs_without_assuming_dust_value() {
        let payload = serde_json::json!({"entries":[{"type":"pre_trade_exposure","entry":{"ts_ms":100,"rows":[
            {"asset":"BTC","open_qty":2,"open_usdt":200,"hedge_qty":-2,"hedge_usdt":-190},
            {"asset":"FTM","open_qty":0.000000003281,"open_usdt":0,"hedge_qty":0,"hedge_usdt":0},
            {"asset":"SLVON","open_qty":0.00911592,"open_usdt":0,"hedge_qty":0,"hedge_usdt":0},
            {"asset":"MISSING","open_qty":1000000,"open_usdt":0,"hedge_qty":-1000000,"hedge_usdt":0},
            {"is_total":true,"open_qty":null,"open_usdt":null,"hedge_qty":null,"hedge_usdt":null}
        ]}}]});
        assert_eq!(
            exposure(&payload).unwrap(),
            Exposure {
                ts: 100,
                long: Some(200.0),
                short: Some(190.0),
                unpriced_assets: vec!["FTM".into(), "MISSING".into(), "SLVON".into()],
            }
        );
    }
    #[test]
    fn entirely_unpriced_side_stays_unknown_and_zero_holdings_stay_zero() {
        let mut payload = serde_json::json!({"entries":[{"type":"pre_trade_exposure","entry":{"ts_ms":100,"rows":[
            {"asset":"SLVON","open_qty":0.00911592,"open_usdt":0,"hedge_qty":0,"hedge_usdt":0}
        ]}}]});
        let value = exposure(&payload).unwrap();
        assert_eq!(value.long, None);
        assert_eq!(value.short, Some(0.0));
        payload["entries"][0]["entry"]["rows"][0]["hedge_qty"] = serde_json::json!(-2);
        assert_eq!(exposure(&payload).unwrap().short, None);
        payload["entries"][0]["entry"]["rows"][0]["open_qty"] = serde_json::json!(0);
        payload["entries"][0]["entry"]["rows"][0]["hedge_qty"] = serde_json::json!(0);
        let value = exposure(&payload).unwrap();
        assert_eq!(value.long, Some(0.0));
        assert_eq!(value.short, Some(0.0));
        assert!(value.unpriced_assets.is_empty());
        payload["entries"][0]["entry"]["rows"][0]["open_usdt"] = serde_json::json!(10);
        assert!(exposure(&payload).is_err());
        payload["entries"][0]["entry"]["rows"][0]["open_usdt"] = Value::Null;
        assert!(exposure(&payload).is_err());
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

const MONITOR_INTERVAL: i64 = 4 * 3_600_000;

fn monitor_boundaries(start: i64, end: i64) -> Result<Vec<i64>> {
    anyhow::ensure!(
        start > 0 && end >= start,
        "invalid monitoring history bounds"
    );
    anyhow::ensure!(
        (end - start) / MONITOR_INTERVAL <= 99_998,
        "monitoring history too large"
    );
    let mut result = vec![start];
    let mut tick = (start.div_euclid(MONITOR_INTERVAL) + 1) * MONITOR_INTERVAL;
    while tick < end {
        result.push(tick);
        tick += MONITOR_INTERVAL;
    }
    if end > start {
        result.push(end);
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MonitorQuery {
    source_id: String,
}

fn monitor_value(account: &Account, time: i64) -> Value {
    let Some(r) = &account.monitor_reconstruction else {
        return serde_json::json!({
            "source_id":account.row.source_id,"start_ms":null,"end_ms":null,
            "generated_at_ms":time,"interval_hours":4,"valuation":"","funding":"actual",
            "complete":false,"loading":account.history_error.is_none(),"unavailable_reason":account.history_error.as_deref().unwrap_or("NAV 历史重算中"),"points":[]
        });
    };
    let realized = account.row.account_kind == "fr";
    let values = if realized {
        &r.history.realized_boundaries
    } else {
        &r.history.boundaries
    };
    let baseline = values.get(&r.start).copied();
    let points = monitor_boundaries(r.start, r.at).ok().and_then(|ticks| {
        ticks
            .into_iter()
            .map(|ts| {
                let value = if ts == r.at && ts != r.start {
                    if realized {
                        r.history.final_realized_net
                    } else {
                        r.history.final_pnl
                    }
                } else {
                    *values.get(&ts)?
                };
                Some(serde_json::json!({"ts_ms":ts,"pnl_quote":value - baseline?}))
            })
            .collect::<Option<Vec<_>>>()
    });
    let stale = time - r.at > 600_000;
    serde_json::json!({
        "source_id":account.row.source_id,"start_ms":r.start,"end_ms":r.at,
        "generated_at_ms":r.at,"interval_hours":4,
        "valuation":if realized {"realized_fifo_after_fees_funding_interest"} else {"nav_latest_trade_after_fees_funding_interest"},
        "funding":"actual","complete":points.is_some() && !stale,"loading":false,
        "unavailable_reason":if points.is_none() {Some("NAV 采样尚未就绪")} else if stale {Some("NAV 历史更新已过期")} else {None},
        "points":points.unwrap_or_default()
    })
}

pub(super) async fn nav_monitor(
    State(state): State<AppState>,
    Query(query): Query<MonitorQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !state.fr_overview.authorized(&headers) {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "integration token required",
            }),
        )
            .into_response());
    }
    let cache = state.fr_overview.cache.read().unwrap();
    let Some(account) = cache.accounts.get(&query.source_id) else {
        return Ok((
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "unknown enabled overview account",
            }),
        )
            .into_response());
    };
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(monitor_value(account, now())),
    )
        .into_response())
}
