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
}

#[derive(FromRow)]
struct Trade {
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
    warnings: Vec<String>,
    alignment: Vec<AlignmentMinute>,
    report: cost::Report,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AlignmentMinute {
    ts: i64,
    spot_quantity: f64,
    futures_quantity: f64,
    net_quantity: f64,
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
    let symbols_sql = format!(
        "SELECT upper(symbol) AS symbol FROM {}.trades WHERE event_time_ms >= $1 AND event_time_ms <= $2 GROUP BY upper(symbol) ORDER BY max(event_time_ms) DESC,upper(symbol)",
        strategy.db_schema
    );
    let mut available_symbols: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(symbols_sql))
        .bind(history_start_ms)
        .bind(end_ms)
        .fetch_all(&state.pool)
        .await?;
    if let Some(parsed) = &parsed {
        for p in &parsed.positions {
            if !available_symbols.contains(&p.symbol) {
                available_symbols.push(p.symbol.clone());
            }
        }
    }
    let symbol = query
        .symbol
        .map(|s| s.trim().to_ascii_uppercase())
        .or_else(|| available_symbols.first().cloned())
        .unwrap_or_default();
    if !symbol.is_empty() && !available_symbols.contains(&symbol) {
        return Ok(not_found("symbol not found in FR history"));
    }
    let sql = format!(
        "SELECT trade_id AS id,order_id,market,side,price::float8 AS price,quantity::float8 AS quantity,fee_amount::float8 AS fee,fee_usdt::float8 AS fee_usdt,COALESCE(fee_asset,'') AS fee_asset,liquidity_role AS liquidity,event_time_ms AS ts FROM {}.trades WHERE upper(symbol)=$1 AND event_time_ms >= $2 AND event_time_ms <= $3 ORDER BY event_time_ms,market,trade_id",
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
    let base = symbol.strip_suffix("USDT").unwrap_or("");
    let (mut spot_balance, mut futures_balance) = inventory.quantities();
    let alignment_start = cost::minute((end_ms - 86_400_000).max(history_start_ms));
    let mut balance_events = BTreeMap::new();
    balance_events.insert(alignment_start, (spot_balance, futures_balance));
    for row in rows {
        let leg = match row.market.as_str() {
            "spot" | "margin" => Leg::Spot,
            "swap" | "linear" | "usdm_futures" | "usdt_futures" => Leg::Futures,
            _ => return Err(anyhow::anyhow!("unsupported FR trade market {}", row.market).into()),
        };
        let side = match row.side.as_str() {
            "buy" => 1,
            "sell" => -1,
            _ => return Err(anyhow::anyhow!("unsupported trade side").into()),
        };
        let fee_asset = row.fee_asset.to_ascii_uppercase();
        let fee_usdt = row.fee_usdt.or_else(|| {
            if row.fee == 0.0 || matches!(fee_asset.as_str(), "USDT" | "USDC" | "USD") {
                Some(row.fee)
            } else if !base.is_empty() && fee_asset == base {
                Some(row.fee * row.price)
            } else {
                None
            }
        });
        let quantity = pnl::spot_swap_position_quantity(
            &strategy.exchange,
            if leg == Leg::Spot {
                pnl::PositionLeg::Spot
            } else {
                pnl::PositionLeg::Futures
            },
            if side == 1 { Side::Buy } else { Side::Sell },
            row.quantity.abs(),
            row.fee,
            &fee_asset,
            base,
        );
        match leg {
            Leg::Spot => spot_balance += f64::from(side) * quantity,
            Leg::Futures => futures_balance += f64::from(side) * quantity,
        }
        balance_events.insert(
            cost::minute(row.ts).max(alignment_start),
            (spot_balance, futures_balance),
        );
        inventory.apply(Fill {
            id: row.id,
            order_id: row.order_id,
            ts: row.ts,
            leg,
            side,
            price: row.price,
            quantity,
            fee_usdt,
            liquidity: row.liquidity,
        })?;
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
    let start = inventory.oldest_pair_time();
    let (premiums, reference_source) = if let Some(start) = start {
        if let Some(bps) = query.reference_bps {
            // A manual scenario is explicitly labeled and never masquerades as market data.
            let values = (cost::minute(start)..=cost::minute(end_ms))
                .step_by(60_000)
                .map(|t| (t, bps / 10_000.0))
                .collect();
            (values, "manual_constant_scenario".into())
        } else {
            let (values, source, issues) =
                crate::fr_premium::load(&state.pool, &strategy.exchange, &symbol, start, end_ms)
                    .await;
            warnings.extend(issues);
            (values, source)
        }
    } else {
        (BTreeMap::new(), "no_paired_position".into())
    };
    let report = tokio::task::spawn_blocking(move || inventory.report(&premiums))
        .await
        .context("join FR cost calculation")?;
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
        warnings,
        alignment,
        report,
    })
    .into_response())
}
