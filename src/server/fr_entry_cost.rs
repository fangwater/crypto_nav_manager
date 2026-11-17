use super::*;
use crate::{
    fifo_pnl::Side,
    fr_entry_cost::{self as cost, Fill, Inventory, Leg, Seed},
};
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CostQuery {
    symbol: Option<String>,
    end_ms: Option<i64>,
    reference_bps: Option<f64>,
    #[serde(default)]
    summary_only: bool,
}

#[derive(FromRow)]
struct Trade {
    symbol: String,
    id: String,
    order_id: String,
    market: String,
    side: String,
    price: f64,
    quantity: f64,
    fee: f64,
    fee_usdt: Option<f64>,
    fee_asset: String,
    liquidity: String,
    ts: i64,
}

impl Trade {
    fn into_fill(self, exchange: &str) -> Result<Fill> {
        let leg = match self.market.as_str() {
            "spot" | "margin" => Leg::Spot,
            "swap" | "linear" | "usdm_futures" | "usdt_futures" => Leg::Futures,
            _ => anyhow::bail!("unsupported FR trade market {}", self.market),
        };
        let side = match self.side.as_str() {
            "buy" => 1,
            "sell" => -1,
            _ => anyhow::bail!("unsupported trade side"),
        };
        let base = self.symbol.strip_suffix("USDT").unwrap_or("");
        let fee_asset = self.fee_asset.to_ascii_uppercase();
        let fee_usdt = self.fee_usdt.or_else(|| {
            if self.fee == 0.0 || matches!(fee_asset.as_str(), "USDT" | "USDC" | "USD") {
                Some(self.fee)
            } else if !base.is_empty() && fee_asset == base {
                Some(self.fee * self.price)
            } else {
                None
            }
        });
        let quantity = pnl::spot_swap_position_quantity(
            exchange,
            if leg == Leg::Spot {
                pnl::PositionLeg::Spot
            } else {
                pnl::PositionLeg::Futures
            },
            if side == 1 { Side::Buy } else { Side::Sell },
            self.quantity.abs(),
            self.fee,
            &fee_asset,
            base,
        );
        Ok(Fill {
            id: self.id,
            order_id: self.order_id,
            ts: self.ts,
            leg,
            side,
            price: self.price,
            quantity,
            fee_usdt,
            liquidity: self.liquidity,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CostResponse {
    strategy_slug: String,
    display_name: String,
    exchange: String,
    symbol: String,
    available_symbols: Vec<String>,
    history_start_ms: i64,
    end_ms: i64,
    last_trade_ms: Option<i64>,
    reference_source: String,
    reference_bps: Option<f64>,
    factual_snapshot_ms: Option<i64>,
    factual_position: Option<FactualPosition>,
    warnings: Vec<String>,
    alignment: Vec<AlignmentMinute>,
    report: cost::Report,
    analysis: crate::fr_entry_analysis::EntryAnalysis,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FactualPosition {
    spot_quantity: f64,
    futures_quantity: f64,
}

struct CurrentHoldings {
    ts_ms: i64,
    positions: Vec<(String, FactualPosition, f64)>,
}

fn parse_current_holdings(payload: &Value, now: i64) -> Result<CurrentHoldings> {
    let entry = payload["entries"]
        .as_array()
        .context("missing snapshot entries")?
        .iter()
        .find(|e| e["type"] == "pre_trade_exposure")
        .context("missing factual positions")?;
    let entry = &entry["entry"];
    let ts_ms = entry["ts_ms"]
        .as_i64()
        .context("missing position timestamp")?;
    anyhow::ensure!(
        (-5_000..=90_000).contains(&(now - ts_ms)),
        "positions are not fresh"
    );
    let rows = entry["rows"].as_array().context("missing position rows")?;
    let mut positions = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for row in rows.iter().filter(|r| r["is_total"] != true) {
        let asset = row["asset"]
            .as_str()
            .context("missing asset")?
            .trim()
            .to_ascii_uppercase();
        anyhow::ensure!(
            !asset.is_empty() && seen.insert(asset.clone()),
            "invalid or duplicate asset"
        );
        let spot = row["open_qty"].as_f64().context("missing spot quantity")?;
        let futures = row["hedge_qty"]
            .as_f64()
            .context("missing futures quantity")?;
        anyhow::ensure!(
            spot.is_finite() && futures.is_finite(),
            "invalid factual quantity"
        );
        // A flat hedge is no longer an open FR position. Spot residual balances
        // do not resurrect it, regardless of historical FIFO residual inventory.
        if futures == 0.0 {
            continue;
        }
        positions.push((
            format!("{asset}USDT"),
            FactualPosition {
                spot_quantity: spot,
                futures_quantity: futures,
            },
            row["hedge_usdt"]
                .as_f64()
                .filter(|v| v.is_finite())
                .map_or(0.0, f64::abs),
        ));
    }
    positions.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    Ok(CurrentHoldings { ts_ms, positions })
}

async fn current_holdings(pool: &PgPool, slug: &str) -> Result<CurrentHoldings> {
    let source = sqlx::query_as::<_, SnapshotStrategyRecord>(
        "SELECT slug,host,config_url,snapshot_url FROM strategy_envs WHERE enabled AND slug=$1",
    )
    .bind(slug)
    .fetch_one(pool)
    .await?;
    let url = source
        .snapshot_url
        .map(Ok)
        .unwrap_or_else(|| snapshot_source_url(&source.host, &source.config_url))?;
    let payload = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(8))
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    parse_current_holdings(&payload, chrono::Utc::now().timestamp_millis())
}

fn holdings_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorResponse {
            error: "当前持仓暂不可用或已过期，请稍后刷新；未使用历史库存替代当前持仓。",
        }),
    )
        .into_response()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AlignmentMinute {
    ts: i64,
    spot_quantity: f64,
    futures_quantity: f64,
    net_quantity: f64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CostRow {
    symbol: String,
    direction: Option<&'static str>,
    spot_quantity: f64,
    futures_quantity: f64,
    summary: cost::Totals,
    paired_liquidity: crate::fr_entry_analysis::LiquidityMix,
    fill_spread_bps: Option<f64>,
    execution_deviation_bps: Option<f64>,
    last_pair_ms: Option<i64>,
    factual_position: FactualPosition,
    covered: crate::fr_entry_analysis::CoveredCosts,
}

impl CostRow {
    fn new(
        symbol: String,
        report: &cost::Report,
        analysis: crate::fr_entry_analysis::EntryAnalysis,
        factual_position: FactualPosition,
    ) -> Self {
        Self {
            symbol,
            direction: report.pairs.first().map(|p| p.direction),
            spot_quantity: report.spot_quantity,
            futures_quantity: report.futures_quantity,
            summary: report.summary.clone(),
            paired_liquidity: analysis.paired_liquidity,
            fill_spread_bps: analysis.fill_spread_bps,
            execution_deviation_bps: analysis.execution_deviation_bps,
            last_pair_ms: analysis.basis.last_pair.map(|p| p.event_ms),
            factual_position,
            covered: analysis.covered,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct OverviewQuery {
    end_ms: Option<i64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CostOverview {
    strategy_slug: String,
    exchange: String,
    end_ms: i64,
    history_start_ms: i64,
    factual_snapshot_ms: i64,
    rows: Vec<CostRow>,
}

// Full-account replays are serialized to bound transient memory and DB load.
static OVERVIEW_REPLAY: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

pub(super) async fn overview(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    AxumPath(slug): AxumPath<String>,
    Query(query): Query<OverviewQuery>,
) -> Result<Response, ApiError> {
    let visible = visible_slugs(&state.pool, &user).await?;
    if !slug_visible(&visible, &slug) {
        return Ok(not_found("strategy not found"));
    }
    let strategy = sqlx::query_as::<_, IntraAnalysisStrategyRecord>(
        "SELECT slug,alias,db_schema,st_ms,exchange,strategy_kind FROM strategy_envs WHERE enabled AND slug=$1 AND strategy_kind='funding_rate'",
    ).bind(&slug).fetch_optional(&state.pool).await?;
    let Some(strategy) = strategy else {
        return Ok(not_found("FR strategy not found"));
    };
    if !valid_schema(&strategy.db_schema) {
        return Err(anyhow::anyhow!("invalid strategy schema").into());
    }
    let now = chrono::Utc::now().timestamp_millis();
    let end_ms = query.end_ms.unwrap_or(now);
    if end_ms < strategy.st_ms || end_ms > now + 60_000 {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "invalid endMs",
            }),
        )
            .into_response());
    }
    let holdings = match current_holdings(&state.pool, &slug).await {
        Ok(holdings) => holdings,
        Err(_) => return Ok(holdings_unavailable()),
    };
    let held_symbols: Vec<String> = holdings
        .positions
        .iter()
        .map(|(s, _, _)| s.clone())
        .collect();
    let _permit = OVERVIEW_REPLAY
        .acquire()
        .await
        .context("FR overview replay permit")?;
    let initial = load_initial_snapshot(&state.pool, &slug)
        .await?
        .filter(|s| s.snapshot_ts_ms <= end_ms);
    let history_start_ms = initial
        .as_ref()
        .map_or(strategy.st_ms, |s| s.snapshot_ts_ms.saturating_add(1));
    let mut inventories: BTreeMap<String, Inventory> = held_symbols
        .iter()
        .map(|s| (s.clone(), Inventory::default()))
        .collect();
    if let Some(initial) = &initial {
        for position in parse_initial_positions(&initial.payload)?
            .positions
            .into_iter()
            .filter(|p| held_symbols.contains(&p.symbol))
        {
            let inventory = inventories.entry(position.symbol).or_default();
            inventory.seed(Seed {
                ts: initial.snapshot_ts_ms,
                leg: Leg::Spot,
                signed_quantity: position.spot_quantity,
            })?;
            inventory.seed(Seed {
                ts: initial.snapshot_ts_ms,
                leg: Leg::Futures,
                signed_quantity: position.futures_quantity,
            })?;
        }
    }
    let sql = format!(
        "SELECT upper(symbol) AS symbol,trade_id AS id,order_id,market,side,price::float8 AS price,quantity::float8 AS quantity,fee_amount::float8 AS fee,fee_usdt::float8 AS fee_usdt,COALESCE(fee_asset,'') AS fee_asset,liquidity_role AS liquidity,event_time_ms AS ts FROM {}.trades WHERE event_time_ms >= $1 AND event_time_ms <= $2 AND upper(symbol)=ANY($3) ORDER BY event_time_ms,market,trade_id",
        strategy.db_schema
    );
    let trades = sqlx::query_as::<_, Trade>(sqlx::AssertSqlSafe(sql))
        .bind(history_start_ms)
        .bind(end_ms)
        .bind(&held_symbols)
        .fetch_all(&state.pool)
        .await?;
    let exchange = strategy.exchange.clone();
    let mut inventories = tokio::task::spawn_blocking(move || {
        for trade in trades {
            let inventory = inventories.entry(trade.symbol.clone()).or_default();
            inventory.apply(trade.into_fill(&exchange)?)?;
        }
        anyhow::Ok(inventories)
    })
    .await
    .context("join FR overview replay")??;
    let mut rows = Vec::with_capacity(inventories.len());
    for (symbol, factual_position, _) in &holdings.positions {
        let inventory = inventories.remove(symbol).unwrap_or_default();
        let symbol = symbol.clone();
        let factual_position = factual_position.clone();
        let premiums = if let Some((start, last_pair)) = inventory.paired_time_range() {
            crate::fr_premium::cached(&state.pool, &strategy.exchange, &symbol, start, last_pair)
                .await?
        } else {
            BTreeMap::new()
        };
        let row = tokio::task::spawn_blocking(move || {
            let report = inventory.report(&premiums);
            // Overview needs retained-inventory metrics, not the whole trading window.
            let analysis =
                crate::fr_entry_analysis::calculate(&report, &[], &premiums, end_ms, false);
            CostRow::new(symbol, &report, analysis, factual_position)
        })
        .await
        .context("join FR overview summary")?;
        rows.push(row);
    }
    Ok(Json(CostOverview {
        strategy_slug: slug,
        exchange: strategy.exchange,
        end_ms,
        history_start_ms,
        factual_snapshot_ms: holdings.ts_ms,
        rows,
    })
    .into_response())
}

pub(super) async fn report(
    State(state): State<AppState>,
    Extension(user): Extension<AuthUser>,
    AxumPath(slug): AxumPath<String>,
    Query(query): Query<CostQuery>,
) -> Result<Response, ApiError> {
    let visible = visible_slugs(&state.pool, &user).await?;
    if !slug_visible(&visible, &slug) {
        return Ok(not_found("strategy not found"));
    }
    let strategy = sqlx::query_as::<_, IntraAnalysisStrategyRecord>(
        "SELECT slug,alias,db_schema,st_ms,exchange,strategy_kind FROM strategy_envs WHERE enabled AND slug=$1 AND strategy_kind='funding_rate'",
    ).bind(&slug).fetch_optional(&state.pool).await?;
    let Some(strategy) = strategy else {
        return Ok(not_found("FR strategy not found"));
    };
    if !valid_schema(&strategy.db_schema) {
        return Err(anyhow::anyhow!("invalid strategy schema").into());
    }
    let now = chrono::Utc::now().timestamp_millis();
    let end_ms = query.end_ms.unwrap_or(now);
    if end_ms < strategy.st_ms
        || end_ms > now + 60_000
        || query
            .reference_bps
            .is_some_and(|v| !v.is_finite() || v.abs() > 10_000.0)
    {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "invalid endMs or referenceBps",
            }),
        )
            .into_response());
    }
    let initial = load_initial_snapshot(&state.pool, &slug)
        .await?
        .filter(|s| s.snapshot_ts_ms <= end_ms);
    let history_start_ms = initial
        .as_ref()
        .map_or(strategy.st_ms, |s| s.snapshot_ts_ms.saturating_add(1));
    let parsed = initial
        .as_ref()
        .map(|s| parse_initial_positions(&s.payload))
        .transpose()?;
    let holdings = match current_holdings(&state.pool, &slug).await {
        Ok(holdings) => holdings,
        Err(_) => return Ok(holdings_unavailable()),
    };
    let available_symbols: Vec<String> = holdings
        .positions
        .iter()
        .map(|(s, _, _)| s.clone())
        .collect();
    let symbol = query
        .symbol
        .map(|s| s.trim().to_ascii_uppercase())
        .or_else(|| available_symbols.first().cloned())
        .unwrap_or_default();
    let factual_position = holdings
        .positions
        .iter()
        .find(|(s, _, _)| *s == symbol)
        .map(|(_, p, _)| p.clone());
    if !symbol.is_empty() && factual_position.is_none() {
        return Ok(not_found("symbol has no current FR position"));
    }
    let sql = format!(
        "SELECT upper(symbol) AS symbol,trade_id AS id,order_id,market,side,price::float8 AS price,quantity::float8 AS quantity,fee_amount::float8 AS fee,fee_usdt::float8 AS fee_usdt,COALESCE(fee_asset,'') AS fee_asset,liquidity_role AS liquidity,event_time_ms AS ts FROM {}.trades WHERE upper(symbol)=$1 AND event_time_ms >= $2 AND event_time_ms <= $3 ORDER BY event_time_ms,market,trade_id",
        strategy.db_schema
    );
    let rows = sqlx::query_as::<_, Trade>(sqlx::AssertSqlSafe(sql))
        .bind(&symbol)
        .bind(history_start_ms)
        .bind(end_ms)
        .fetch_all(&state.pool)
        .await?;
    let last_trade_ms = rows.last().map(|r| r.ts);
    let mut warnings = vec!["按两腿各自数量 FIFO 重建当前剩余库存，再跨腿配对；不代表原始订单一一对应。成交重建数量未包含未入库划转、借币利息等变动。".into()];
    let mut inventory = Inventory::default();
    if let Some(parsed) = &parsed {
        for p in parsed.positions.iter().filter(|p| p.symbol == symbol) {
            let ts = initial.as_ref().unwrap().snapshot_ts_ms;
            inventory.seed(Seed {
                ts,
                leg: Leg::Spot,
                signed_quantity: p.spot_quantity,
            })?;
            inventory.seed(Seed {
                ts,
                leg: Leg::Futures,
                signed_quantity: p.futures_quantity,
            })?;
        }
        warnings.push(
            "期初快照的数量参加 FIFO，但快照估值不是原始建仓价；未平掉的期初库存成本保留未知。"
                .into(),
        );
    }
    let (mut spot_balance, mut futures_balance) = inventory.quantities();
    let alignment_start = cost::minute((end_ms - 86_400_000).max(history_start_ms));
    let mut balance_events = BTreeMap::new();
    balance_events.insert(alignment_start, (spot_balance, futures_balance));
    let mut fills = Vec::with_capacity(rows.len());
    for row in rows {
        let fill = row.into_fill(&strategy.exchange)?;
        match fill.leg {
            Leg::Spot => spot_balance += f64::from(fill.side) * fill.quantity,
            Leg::Futures => futures_balance += f64::from(fill.side) * fill.quantity,
        }
        balance_events.insert(
            cost::minute(fill.ts).max(alignment_start),
            (spot_balance, futures_balance),
        );
        inventory.apply(fill.clone())?;
        fills.push(fill);
    }
    let mut alignment = Vec::new();
    let mut latest = (0.0, 0.0);
    for ts in (alignment_start..=cost::minute(end_ms)).step_by(60_000) {
        if let Some(&balance) = balance_events.get(&ts) {
            latest = balance;
        }
        alignment.push(AlignmentMinute {
            ts,
            spot_quantity: latest.0,
            futures_quantity: latest.1,
            net_quantity: latest.0 + latest.1,
        });
    }
    let range = inventory.paired_time_range();
    let (premiums, reference_source) = if let Some((start, last_pair)) = range {
        if let Some(bps) = query.reference_bps {
            // A manual scenario is explicitly labeled and never masquerades as market data.
            let values = (cost::minute(start)..=cost::minute(end_ms))
                .step_by(60_000)
                .map(|t| (t, bps / 10_000.0))
                .collect();
            (values, "manual_constant_scenario".into())
        } else {
            let windows = query.summary_only.then(|| inventory.reference_windows());
            let (values, source, issues) = crate::fr_premium::load(
                &state.pool,
                &strategy.exchange,
                &symbol,
                start,
                if query.summary_only {
                    last_pair
                } else {
                    end_ms
                },
                &[last_pair, cost::minute(end_ms) - cost::MINUTE_MS],
                windows.as_deref(),
            )
            .await;
            warnings.extend(issues);
            (values, source)
        }
    } else {
        (BTreeMap::new(), "no_paired_position".into())
    };
    let scenario = query.reference_bps.is_some();
    let (report, analysis) = tokio::task::spawn_blocking(move || {
        let report = inventory.report(&premiums);
        let analysis =
            crate::fr_entry_analysis::calculate(&report, &fills, &premiums, end_ms, scenario);
        (report, analysis)
    })
    .await
    .context("join FR cost calculation")?;
    if query.summary_only {
        let Some(factual_position) = factual_position else {
            return Ok(not_found("no current FR position"));
        };
        return Ok(Json(CostRow::new(symbol, &report, analysis, factual_position)).into_response());
    }
    if report.summary.cost_covered_quantity < report.summary.quantity {
        warnings.push("部分库存缺少原始成交价、手续费折算或所需分钟参考；完整总成本显示为未知，不能将缺失部分按零计入。".into());
    }
    warnings.push("参考基差使用 premium index 分钟收盘值；跨分钟取所覆盖分钟等权均值，再按配对名义本金汇总。它是事后估算，包含指数口径差异与对冲等待，不能解释为精确滑点。".into());
    Ok(Json(CostResponse {
        strategy_slug: slug.clone(),
        display_name: strategy.alias.unwrap_or(slug),
        exchange: strategy.exchange,
        symbol,
        available_symbols,
        history_start_ms,
        end_ms,
        last_trade_ms,
        reference_source,
        reference_bps: query.reference_bps,
        factual_snapshot_ms: Some(holdings.ts_ms),
        factual_position,
        warnings,
        alignment,
        report,
        analysis,
    })
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot(rows: Value, ts: i64) -> Value {
        json!({"entries":[{"type":"pre_trade_exposure","entry":{"ts_ms":ts,"rows":rows}}]})
    }

    #[test]
    fn fr_current_holdings_excludes_closed_hedges_including_spot_residuals() {
        let payload = snapshot(
            json!([
                {"asset":"OPEN","open_qty":100,"hedge_qty":-100,"hedge_usdt":-500},
                {"asset":"REVERSE","open_qty":-20,"hedge_qty":20,"hedge_usdt":200},
                {"asset":"CLOSED","open_qty":0,"hedge_qty":0},
                {"asset":"RESIDUAL","open_qty":0.001,"hedge_qty":0},
                {"is_total":true,"open_qty":null,"hedge_qty":null}
            ]),
            100_000,
        );
        let holdings = parse_current_holdings(&payload, 101_000).unwrap();
        assert_eq!(
            holdings
                .positions
                .iter()
                .map(|p| p.0.as_str())
                .collect::<Vec<_>>(),
            vec!["OPENUSDT", "REVERSEUSDT"]
        );
        assert_eq!(holdings.ts_ms, 100_000);
    }

    #[test]
    fn fr_current_holdings_does_not_infer_flatness_from_unpriced_or_small_hedges() {
        let payload = snapshot(
            json!([
                {"asset":"UNPRICED","open_qty":100,"hedge_qty":-100,"hedge_usdt":0},
                {"asset":"SMALL","open_qty":0,"hedge_qty":0.00000000001}
            ]),
            100_000,
        );
        assert_eq!(
            parse_current_holdings(&payload, 101_000)
                .unwrap()
                .positions
                .len(),
            2
        );
    }

    #[test]
    fn fr_current_holdings_unavailable_is_not_an_empty_account() {
        assert!(parse_current_holdings(&json!({"entries":[]}), 100_000).is_err());
        let malformed = snapshot(
            json!([{"asset":"BTC","open_qty":1,"hedge_qty":null}]),
            100_000,
        );
        assert!(parse_current_holdings(&malformed, 100_000).is_err());
        let valid_empty = snapshot(json!([]), 100_000);
        assert!(
            parse_current_holdings(&valid_empty, 101_000)
                .unwrap()
                .positions
                .is_empty()
        );
        assert!(parse_current_holdings(&valid_empty, 190_001).is_err());
        assert!(parse_current_holdings(&valid_empty, 94_999).is_err());
    }
}
