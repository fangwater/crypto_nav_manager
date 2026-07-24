use crate::{
    contract_multipliers::ContractMultiplierBook,
    fifo_pnl::{FifoPnl, PnlSnapshot, Side},
    mark_prices::{MarkPriceCache, MarkPriceExchange},
};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use sqlx::{AssertSqlSafe, FromRow, PgPool};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

const STABLECOINS: [&str; 3] = ["USDT", "USDC", "USD"];
// Binance records Spot MM2 maker commission as zero and settles the rebate
// through the next-hour wallet distribution.
const BINANCE_INTRA_SPOT_MAKER_REBATE_RATE: f64 = 0.4 / 10_000.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PnlSourceKind {
    Intra,
    FundingRate,
    MarketMaking,
}

impl PnlSourceKind {
    pub fn for_strategy(strategy_kind: &str, exchange: &str, account_mode: &str) -> Option<Self> {
        match (strategy_kind, exchange, account_mode) {
            ("intra_exchange", "binance", "usdm_futures")
            | ("intra_exchange", "bybit" | "gate", "unified") => Some(Self::Intra),
            ("funding_rate", "bybit" | "gate", "unified") => Some(Self::FundingRate),
            ("market_making", "binance", "usdm_futures")
            | ("market_making", "bybit" | "gate" | "okx", "unified") => Some(Self::MarketMaking),
            _ => None,
        }
    }

    fn adapter_name(self) -> &'static str {
        match self {
            Self::Intra | Self::FundingRate => "spot_swap_history_v1",
            Self::MarketMaking => "market_making_futures_v1",
        }
    }

    fn interest_included(self, exchange: &str) -> bool {
        matches!(
            (self, exchange),
            (Self::Intra | Self::FundingRate, "bybit" | "gate")
        )
    }

    fn exposure(self, spot_position_usdt: f64, futures_position_usdt: f64) -> f64 {
        spot_position_usdt + futures_position_usdt
    }
}

#[derive(Clone, Debug)]
pub struct NormalizedTrade {
    pub symbol: String,
    pub side: Side,
    pub leg: PositionLeg,
    pub price: f64,
    pub amount_u: f64,
    pub fee_usdt: Option<f64>,
    pub ts: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PositionLeg {
    Spot,
    Futures,
}

#[derive(Clone, Debug)]
pub struct FundingEvent {
    pub symbol: String,
    pub amount_usdt: f64,
    pub ts: i64,
}

#[derive(Clone, Debug)]
pub struct InterestEvent {
    pub symbol: String,
    pub cost_usdt: Option<f64>,
    pub ts: i64,
}

#[derive(Clone, Debug, Default)]
pub struct PnlInputs {
    pub trades: Vec<NormalizedTrade>,
    pub funding: Vec<FundingEvent>,
    pub interest: Vec<InterestEvent>,
}

#[derive(Clone, Debug)]
pub struct PnlCalculation {
    pub source: PnlSourceKind,
    pub exchange: String,
    pub strategy_start_ms: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub selected_symbols: Vec<String>,
    pub max_points: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PnlResponse {
    pub strategy_start_ms: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub selected_symbols: Vec<String>,
    pub available_symbols: Vec<String>,
    pub summary: PnlSummary,
    pub symbols: Vec<SymbolPnlSummary>,
    pub points: Vec<PnlPoint>,
    pub symbol_points: Vec<SymbolPnlSeries>,
    pub source: PnlSourceInfo,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PnlSummary {
    pub trade_count: u64,
    pub volume_usdt: f64,
    pub fee_before_pnl_usdt: f64,
    pub trading_fee_usdt: f64,
    pub fee_after_pnl_usdt: f64,
    pub funding_pnl_usdt: f64,
    pub interest_cost_usdt: f64,
    pub floating_pnl_usdt: f64,
    pub total_pnl_usdt: f64,
    pub return_bps_on_volume: f64,
    pub open_amount_usdt: f64,
    pub unconverted_fee_count: u64,
    pub unconverted_interest_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolPnlSummary {
    pub symbol: String,
    #[serde(flatten)]
    pub pnl: PnlSummary,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SymbolPnlSeries {
    pub symbol: String,
    pub points: Vec<PnlPoint>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PnlPoint {
    pub ts: i64,
    pub fee_before_pnl_usdt: f64,
    pub fee_after_pnl_usdt: f64,
    pub funding_pnl_usdt: f64,
    pub interest_cost_usdt: f64,
    pub floating_pnl_usdt: f64,
    pub total_pnl_usdt: f64,
    pub spot_position_usdt: f64,
    pub futures_position_usdt: f64,
    pub exposure_usdt: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PnlSourceInfo {
    pub adapter: &'static str,
    pub loaded_trade_rows: usize,
    pub loaded_funding_rows: usize,
    pub loaded_interest_rows: usize,
    pub returned_points: usize,
    pub returned_symbol_points: usize,
    pub sampled: bool,
    pub interest_included: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct Metrics {
    trade_count: u64,
    volume_usdt: f64,
    fee_before_pnl_usdt: f64,
    trading_fee_usdt: f64,
    fee_after_pnl_usdt: f64,
    funding_pnl_usdt: f64,
    interest_cost_usdt: f64,
    floating_pnl_usdt: f64,
    total_pnl_usdt: f64,
    open_amount_usdt: f64,
    spot_position_usdt: f64,
    futures_position_usdt: f64,
    unconverted_fee_count: u64,
    unconverted_interest_count: u64,
}

impl Metrics {
    fn difference(self, baseline: Self) -> PnlSummary {
        let volume = self.volume_usdt - baseline.volume_usdt;
        let total = self.total_pnl_usdt - baseline.total_pnl_usdt;
        PnlSummary {
            trade_count: self.trade_count.saturating_sub(baseline.trade_count),
            volume_usdt: clean_zero(volume),
            fee_before_pnl_usdt: clean_zero(
                self.fee_before_pnl_usdt - baseline.fee_before_pnl_usdt,
            ),
            trading_fee_usdt: clean_zero(self.trading_fee_usdt - baseline.trading_fee_usdt),
            fee_after_pnl_usdt: clean_zero(self.fee_after_pnl_usdt - baseline.fee_after_pnl_usdt),
            funding_pnl_usdt: clean_zero(self.funding_pnl_usdt - baseline.funding_pnl_usdt),
            interest_cost_usdt: clean_zero(self.interest_cost_usdt - baseline.interest_cost_usdt),
            floating_pnl_usdt: clean_zero(self.floating_pnl_usdt - baseline.floating_pnl_usdt),
            total_pnl_usdt: clean_zero(total),
            return_bps_on_volume: if volume.abs() > f64::EPSILON {
                total / volume * 10_000.0
            } else {
                0.0
            },
            open_amount_usdt: clean_zero(self.open_amount_usdt),
            unconverted_fee_count: self
                .unconverted_fee_count
                .saturating_sub(baseline.unconverted_fee_count),
            unconverted_interest_count: self
                .unconverted_interest_count
                .saturating_sub(baseline.unconverted_interest_count),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct SymbolState {
    spot_fifo: FifoPnl,
    futures_fifo: FifoPnl,
    funding_pnl_usdt: f64,
    interest_cost_usdt: f64,
    trade_count: u64,
    volume_usdt: f64,
    spot_position_usdt: f64,
    futures_position_usdt: f64,
    unconverted_fee_count: u64,
    unconverted_interest_count: u64,
    spot_snapshot: Option<PnlSnapshot>,
    futures_snapshot: Option<PnlSnapshot>,
}

impl SymbolState {
    fn apply_trade(&mut self, trade: &NormalizedTrade) -> Result<()> {
        let fee = trade.fee_usdt.unwrap_or_else(|| {
            self.unconverted_fee_count += 1;
            0.0
        });
        let (fifo, cached_snapshot) = match trade.leg {
            PositionLeg::Spot => (&mut self.spot_fifo, &mut self.spot_snapshot),
            PositionLeg::Futures => (&mut self.futures_fifo, &mut self.futures_snapshot),
        };
        fifo.apply_fill(trade.side, trade.price, trade.amount_u, fee)
            .context("apply normalized trade to venue FIFO")?;
        self.trade_count += 1;
        self.volume_usdt += trade.amount_u;
        let signed_amount_u = match trade.side {
            Side::Buy => trade.amount_u,
            Side::Sell => -trade.amount_u,
        };
        match trade.leg {
            PositionLeg::Spot => self.spot_position_usdt += signed_amount_u,
            PositionLeg::Futures => self.futures_position_usdt += signed_amount_u,
        }
        *cached_snapshot = Some(
            fifo.snapshot(trade.price, trade.price)
                .context("mark venue FIFO at the latest trade price")?,
        );
        Ok(())
    }

    fn metrics(&self) -> Metrics {
        let spot = self.spot_snapshot.unwrap_or_default();
        let futures = self.futures_snapshot.unwrap_or_default();
        Metrics {
            trade_count: self.trade_count,
            volume_usdt: self.volume_usdt,
            fee_before_pnl_usdt: spot.gross_realized_pnl + futures.gross_realized_pnl,
            trading_fee_usdt: spot.cumulative_fees + futures.cumulative_fees,
            fee_after_pnl_usdt: spot.realized_pnl + futures.realized_pnl,
            funding_pnl_usdt: self.funding_pnl_usdt,
            interest_cost_usdt: self.interest_cost_usdt,
            floating_pnl_usdt: spot.floating_pnl + futures.floating_pnl,
            total_pnl_usdt: spot.total_pnl + futures.total_pnl + self.funding_pnl_usdt
                - self.interest_cost_usdt,
            open_amount_usdt: clean_zero(self.spot_position_usdt + self.futures_position_usdt),
            spot_position_usdt: self.spot_position_usdt,
            futures_position_usdt: self.futures_position_usdt,
            unconverted_fee_count: self.unconverted_fee_count,
            unconverted_interest_count: self.unconverted_interest_count,
        }
    }
}

#[derive(Clone, Debug)]
enum PnlEvent {
    Trade(NormalizedTrade),
    Funding(FundingEvent),
    Interest(InterestEvent),
}

impl PnlEvent {
    fn ts(&self) -> i64 {
        match self {
            Self::Trade(value) => value.ts,
            Self::Funding(value) => value.ts,
            Self::Interest(value) => value.ts,
        }
    }

    fn symbol(&self) -> &str {
        match self {
            Self::Trade(value) => &value.symbol,
            Self::Funding(value) => &value.symbol,
            Self::Interest(value) => &value.symbol,
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Trade(_) => 0,
            Self::Funding(_) => 1,
            Self::Interest(_) => 2,
        }
    }
}

pub async fn load_inputs(
    pool: &PgPool,
    source: PnlSourceKind,
    schema: &str,
    exchange: &str,
    mark_prices: &MarkPriceCache,
    strategy_start_ms: i64,
    end_ms: i64,
) -> Result<PnlInputs> {
    validate_identifier(schema)?;
    match (source, exchange) {
        (PnlSourceKind::Intra, "binance") => {
            load_binance_intra_inputs(pool, schema, mark_prices, strategy_start_ms, end_ms).await
        }
        (PnlSourceKind::Intra | PnlSourceKind::FundingRate, "bybit" | "gate") => {
            load_spot_swap_inputs(pool, schema, exchange, strategy_start_ms, end_ms).await
        }
        (PnlSourceKind::MarketMaking, exchange) => {
            load_market_making_inputs(pool, schema, exchange, strategy_start_ms, end_ms).await
        }
        _ => bail!("unsupported PnL source {source:?} for exchange {exchange}"),
    }
}

pub fn calculate(inputs: PnlInputs, request: PnlCalculation) -> Result<PnlResponse> {
    if request.start_ms < request.strategy_start_ms {
        bail!("start_ms must be greater than or equal to strategy st_ms");
    }
    if request.end_ms < request.start_ms {
        bail!("end_ms must be greater than or equal to start_ms");
    }

    let loaded_trade_rows = inputs.trades.len();
    let loaded_funding_rows = inputs.funding.len();
    let loaded_interest_rows = inputs.interest.len();
    let mut available = BTreeSet::new();
    for trade in &inputs.trades {
        available.insert(trade.symbol.clone());
    }
    for funding in &inputs.funding {
        available.insert(funding.symbol.clone());
    }
    for interest in &inputs.interest {
        available.insert(interest.symbol.clone());
    }
    let available_symbols = available.into_iter().collect::<Vec<_>>();

    let selected_symbols = if request.selected_symbols.is_empty() {
        available_symbols.clone()
    } else {
        let available = available_symbols.iter().cloned().collect::<HashSet<_>>();
        let mut selected = request
            .selected_symbols
            .into_iter()
            .map(|symbol| symbol.to_ascii_uppercase())
            .filter(|symbol| available.contains(symbol))
            .collect::<Vec<_>>();
        selected.sort();
        selected.dedup();
        selected
    };
    if selected_symbols.is_empty() && !available_symbols.is_empty() {
        bail!("none of the requested symbols exist in the selected time range");
    }
    let selected_set = selected_symbols.iter().cloned().collect::<HashSet<_>>();

    let mut events =
        Vec::with_capacity(loaded_trade_rows + loaded_funding_rows + loaded_interest_rows);
    events.extend(inputs.trades.into_iter().map(PnlEvent::Trade));
    events.extend(inputs.funding.into_iter().map(PnlEvent::Funding));
    events.extend(inputs.interest.into_iter().map(PnlEvent::Interest));
    events.sort_by(|left, right| {
        left.ts()
            .cmp(&right.ts())
            .then_with(|| left.rank().cmp(&right.rank()))
            .then_with(|| left.symbol().cmp(right.symbol()))
    });

    let mut states = available_symbols
        .iter()
        .cloned()
        .map(|symbol| (symbol, SymbolState::default()))
        .collect::<HashMap<_, _>>();

    let split = events.partition_point(|event| event.ts() < request.start_ms);
    for event in &events[..split] {
        apply_event(&mut states, event)?;
    }
    let baselines = states
        .iter()
        .map(|(symbol, state)| (symbol.clone(), state.metrics()))
        .collect::<HashMap<_, _>>();

    let mut points = vec![aggregate_point(
        request.start_ms,
        &states,
        &baselines,
        &selected_set,
        request.source,
    )];
    let mut points_by_symbol = selected_symbols
        .iter()
        .cloned()
        .map(|symbol| {
            let start_point = symbol_point(
                request.start_ms,
                &symbol,
                &states,
                &baselines,
                request.source,
            );
            (symbol, vec![start_point])
        })
        .collect::<HashMap<_, _>>();
    for event in &events[split..] {
        if event.ts() > request.end_ms {
            break;
        }
        apply_event(&mut states, event)?;
        if selected_set.contains(event.symbol()) {
            push_or_replace_point(
                &mut points,
                aggregate_point(
                    event.ts(),
                    &states,
                    &baselines,
                    &selected_set,
                    request.source,
                ),
            );
            if let Some(symbol_points) = points_by_symbol.get_mut(event.symbol()) {
                push_or_replace_point(
                    symbol_points,
                    symbol_point(
                        event.ts(),
                        event.symbol(),
                        &states,
                        &baselines,
                        request.source,
                    ),
                );
            }
        }
    }
    push_or_replace_point(
        &mut points,
        aggregate_point(
            request.end_ms,
            &states,
            &baselines,
            &selected_set,
            request.source,
        ),
    );
    for symbol in &selected_symbols {
        if let Some(symbol_points) = points_by_symbol.get_mut(symbol) {
            push_or_replace_point(
                symbol_points,
                symbol_point(request.end_ms, symbol, &states, &baselines, request.source),
            );
        }
    }

    let mut symbols = available_symbols
        .iter()
        .map(|symbol| {
            let current = states
                .get(symbol)
                .map(SymbolState::metrics)
                .unwrap_or_default();
            let baseline = baselines.get(symbol).copied().unwrap_or_default();
            SymbolPnlSummary {
                symbol: symbol.clone(),
                pnl: current.difference(baseline),
            }
        })
        .collect::<Vec<_>>();
    symbols.sort_by(|left, right| {
        right
            .pnl
            .total_pnl_usdt
            .partial_cmp(&left.pnl.total_pnl_usdt)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.symbol.cmp(&right.symbol))
    });

    let mut summary = aggregate_summary(&symbols, &selected_set);
    summary.open_amount_usdt = points
        .last()
        .map(|point| point.exposure_usdt)
        .unwrap_or_default();
    let original_points = points.len();
    let points = downsample_extrema(points, request.max_points.max(2));
    let returned_points = points.len();

    let symbol_max_points = request.max_points.clamp(100, 800);
    let mut sampled_symbol_points = false;
    let mut returned_symbol_points = 0;
    let symbol_points = selected_symbols
        .iter()
        .map(|symbol| {
            let original = points_by_symbol.remove(symbol).unwrap_or_default();
            let original_len = original.len();
            let points = downsample_extrema(original, symbol_max_points);
            sampled_symbol_points |= points.len() < original_len;
            returned_symbol_points += points.len();
            SymbolPnlSeries {
                symbol: symbol.clone(),
                points,
            }
        })
        .collect();

    Ok(PnlResponse {
        strategy_start_ms: request.strategy_start_ms,
        start_ms: request.start_ms,
        end_ms: request.end_ms,
        selected_symbols,
        available_symbols,
        summary,
        symbols,
        points,
        symbol_points,
        source: PnlSourceInfo {
            adapter: request.source.adapter_name(),
            loaded_trade_rows,
            loaded_funding_rows,
            loaded_interest_rows,
            returned_points,
            returned_symbol_points,
            sampled: returned_points < original_points || sampled_symbol_points,
            interest_included: request.source.interest_included(&request.exchange),
        },
    })
}

fn apply_event(states: &mut HashMap<String, SymbolState>, event: &PnlEvent) -> Result<()> {
    let state = states
        .get_mut(event.symbol())
        .context("PnL event symbol is missing from state map")?;
    match event {
        PnlEvent::Trade(trade) => state.apply_trade(trade),
        PnlEvent::Funding(funding) => {
            state.funding_pnl_usdt += funding.amount_usdt;
            Ok(())
        }
        PnlEvent::Interest(interest) => {
            if let Some(cost) = interest.cost_usdt {
                state.interest_cost_usdt += cost;
            } else {
                state.unconverted_interest_count += 1;
            }
            Ok(())
        }
    }
}

fn aggregate_point(
    ts: i64,
    states: &HashMap<String, SymbolState>,
    baselines: &HashMap<String, Metrics>,
    selected: &HashSet<String>,
    source: PnlSourceKind,
) -> PnlPoint {
    let mut point = PnlPoint {
        ts,
        ..PnlPoint::default()
    };
    for symbol in selected {
        let current = states
            .get(symbol)
            .map(SymbolState::metrics)
            .unwrap_or_default();
        let baseline = baselines.get(symbol).copied().unwrap_or_default();
        point.fee_before_pnl_usdt += current.fee_before_pnl_usdt - baseline.fee_before_pnl_usdt;
        point.fee_after_pnl_usdt += current.fee_after_pnl_usdt - baseline.fee_after_pnl_usdt;
        point.funding_pnl_usdt += current.funding_pnl_usdt - baseline.funding_pnl_usdt;
        point.interest_cost_usdt += current.interest_cost_usdt - baseline.interest_cost_usdt;
        point.floating_pnl_usdt += current.floating_pnl_usdt - baseline.floating_pnl_usdt;
        point.total_pnl_usdt += current.total_pnl_usdt - baseline.total_pnl_usdt;
        point.spot_position_usdt += current.spot_position_usdt;
        point.futures_position_usdt += current.futures_position_usdt;
        point.exposure_usdt +=
            source.exposure(current.spot_position_usdt, current.futures_position_usdt);
    }
    point.fee_before_pnl_usdt = clean_zero(point.fee_before_pnl_usdt);
    point.fee_after_pnl_usdt = clean_zero(point.fee_after_pnl_usdt);
    point.funding_pnl_usdt = clean_zero(point.funding_pnl_usdt);
    point.interest_cost_usdt = clean_zero(point.interest_cost_usdt);
    point.floating_pnl_usdt = clean_zero(point.floating_pnl_usdt);
    point.total_pnl_usdt = clean_zero(point.total_pnl_usdt);
    point.spot_position_usdt = clean_zero(point.spot_position_usdt);
    point.futures_position_usdt = clean_zero(point.futures_position_usdt);
    point.exposure_usdt = clean_zero(point.exposure_usdt);
    point
}

fn symbol_point(
    ts: i64,
    symbol: &str,
    states: &HashMap<String, SymbolState>,
    baselines: &HashMap<String, Metrics>,
    source: PnlSourceKind,
) -> PnlPoint {
    let current = states
        .get(symbol)
        .map(SymbolState::metrics)
        .unwrap_or_default();
    let baseline = baselines.get(symbol).copied().unwrap_or_default();
    PnlPoint {
        ts,
        fee_before_pnl_usdt: clean_zero(current.fee_before_pnl_usdt - baseline.fee_before_pnl_usdt),
        fee_after_pnl_usdt: clean_zero(current.fee_after_pnl_usdt - baseline.fee_after_pnl_usdt),
        funding_pnl_usdt: clean_zero(current.funding_pnl_usdt - baseline.funding_pnl_usdt),
        interest_cost_usdt: clean_zero(current.interest_cost_usdt - baseline.interest_cost_usdt),
        floating_pnl_usdt: clean_zero(current.floating_pnl_usdt - baseline.floating_pnl_usdt),
        total_pnl_usdt: clean_zero(current.total_pnl_usdt - baseline.total_pnl_usdt),
        spot_position_usdt: clean_zero(current.spot_position_usdt),
        futures_position_usdt: clean_zero(current.futures_position_usdt),
        exposure_usdt: clean_zero(
            source.exposure(current.spot_position_usdt, current.futures_position_usdt),
        ),
    }
}

fn aggregate_summary(symbols: &[SymbolPnlSummary], selected: &HashSet<String>) -> PnlSummary {
    let mut total = PnlSummary::default();
    for row in symbols.iter().filter(|row| selected.contains(&row.symbol)) {
        total.trade_count += row.pnl.trade_count;
        total.volume_usdt += row.pnl.volume_usdt;
        total.fee_before_pnl_usdt += row.pnl.fee_before_pnl_usdt;
        total.trading_fee_usdt += row.pnl.trading_fee_usdt;
        total.fee_after_pnl_usdt += row.pnl.fee_after_pnl_usdt;
        total.funding_pnl_usdt += row.pnl.funding_pnl_usdt;
        total.interest_cost_usdt += row.pnl.interest_cost_usdt;
        total.floating_pnl_usdt += row.pnl.floating_pnl_usdt;
        total.total_pnl_usdt += row.pnl.total_pnl_usdt;
        total.open_amount_usdt += row.pnl.open_amount_usdt;
        total.unconverted_fee_count += row.pnl.unconverted_fee_count;
        total.unconverted_interest_count += row.pnl.unconverted_interest_count;
    }
    total.return_bps_on_volume = if total.volume_usdt.abs() > f64::EPSILON {
        total.total_pnl_usdt / total.volume_usdt * 10_000.0
    } else {
        0.0
    };
    total
}

fn push_or_replace_point(points: &mut Vec<PnlPoint>, point: PnlPoint) {
    if let Some(last) = points.last_mut()
        && last.ts == point.ts
    {
        *last = point;
        return;
    }
    points.push(point);
}

fn downsample_extrema(points: Vec<PnlPoint>, max_points: usize) -> Vec<PnlPoint> {
    const VALUE_SELECTORS: [fn(&PnlPoint) -> f64; 4] = [
        |point| point.total_pnl_usdt,
        |point| point.spot_position_usdt,
        |point| point.futures_position_usdt,
        |point| point.exposure_usdt,
    ];

    if points.len() <= max_points || max_points < 10 {
        return points;
    }
    let interior = &points[1..points.len() - 1];
    let bucket_count = ((max_points - 2) / (VALUE_SELECTORS.len() * 2)).max(1);
    let bucket_size = interior.len().div_ceil(bucket_count);
    let mut sampled = Vec::with_capacity(max_points);
    sampled.push(points[0]);
    for bucket in interior.chunks(bucket_size) {
        let mut extrema = Vec::with_capacity(VALUE_SELECTORS.len() * 2);
        for value in VALUE_SELECTORS {
            if let Some(point) = bucket.iter().min_by(|left, right| {
                value(left)
                    .partial_cmp(&value(right))
                    .unwrap_or(Ordering::Equal)
            }) {
                extrema.push(*point);
            }
            if let Some(point) = bucket.iter().max_by(|left, right| {
                value(left)
                    .partial_cmp(&value(right))
                    .unwrap_or(Ordering::Equal)
            }) {
                extrema.push(*point);
            }
        }
        extrema.sort_by_key(|point| point.ts);
        extrema.dedup_by_key(|point| point.ts);
        sampled.extend(extrema);
    }
    sampled.push(*points.last().expect("non-empty points"));
    sampled
}

fn clean_zero(value: f64) -> f64 {
    if value.abs() < 1e-12 { 0.0 } else { value }
}

fn validate_identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    {
        bail!("invalid PostgreSQL identifier");
    }
    Ok(())
}

#[derive(Debug, FromRow)]
struct SpotSwapTradeRow {
    key: String,
    symbol: String,
    side: String,
    price: f64,
    amount_u: f64,
    fee: f64,
    commission_asset: String,
    ts: i64,
}

#[derive(Debug, FromRow)]
struct SpotSwapFundingRow {
    symbol: String,
    funding: f64,
    ts: i64,
}

#[derive(Debug, FromRow)]
struct SpotSwapInterestRow {
    currency: String,
    interest: f64,
    ts: i64,
}

#[derive(Debug, FromRow)]
struct BinanceIntraTradeRow {
    market: String,
    liquidity_role: String,
    symbol: String,
    side: String,
    price: f64,
    amount_u: f64,
    fee: f64,
    commission_asset: String,
    ts: i64,
}

#[derive(Debug, FromRow)]
struct BinanceIntraFundingRow {
    symbol: String,
    amount_usdt: f64,
    ts: i64,
}

#[derive(Debug, FromRow)]
struct MarketMakingTradeRow {
    symbol: String,
    side: String,
    price: f64,
    quantity: f64,
    quote_quantity: Option<f64>,
    fee_usdt: Option<f64>,
    ts: i64,
}

#[derive(Debug, FromRow)]
struct MarketMakingFundingRow {
    symbol: String,
    amount_usdt: f64,
    ts: i64,
}

async fn load_binance_intra_inputs(
    pool: &PgPool,
    schema: &str,
    mark_prices: &MarkPriceCache,
    strategy_start_ms: i64,
    end_ms: i64,
) -> Result<PnlInputs> {
    let trade_sql = format!(
        r#"SELECT market, liquidity_role, symbol, side, price::float8 AS price,
                  COALESCE(quote_quantity, price * quantity)::float8 AS amount_u,
                  fee_amount::float8 AS fee,
                  COALESCE(fee_asset, 'USDT') AS commission_asset,
                  event_time_ms AS ts
           FROM {schema}.trades
           WHERE event_time_ms >= $1 AND event_time_ms <= $2
           ORDER BY event_time_ms, symbol, trade_id"#
    );
    let funding_sql = format!(
        r#"SELECT symbol, COALESCE(amount_usdt, amount)::float8 AS amount_usdt,
                  event_time_ms AS ts
           FROM {schema}.funding
           WHERE symbol IS NOT NULL
             AND event_time_ms >= $1 AND event_time_ms <= $2
           ORDER BY event_time_ms, symbol, record_id"#
    );

    let rows = sqlx::query_as::<_, BinanceIntraTradeRow>(AssertSqlSafe(trade_sql))
        .bind(strategy_start_ms)
        .bind(end_ms)
        .fetch_all(pool)
        .await
        .with_context(|| format!("load Binance intra trades from {schema}"))?;
    let trades = rows
        .into_iter()
        .map(|row| {
            let side = match row.side.as_str() {
                "buy" => Side::Buy,
                "sell" => Side::Sell,
                _ => bail!("unsupported Binance intra trade side {:?}", row.side),
            };
            let symbol = row.symbol.to_ascii_uppercase();
            if !row.amount_u.is_finite() || row.amount_u <= 0.0 {
                bail!("invalid Binance intra amount_u: {}", row.amount_u);
            }
            let observed_fee_usdt = binance_fee_cost_usdt(
                row.fee,
                &row.commission_asset,
                row.price,
                &symbol,
                mark_prices,
            );
            let fee_usdt = binance_intra_fee_usdt(
                &row.market,
                &row.liquidity_role,
                row.amount_u,
                observed_fee_usdt,
            );
            let leg = match row.market.as_str() {
                "spot" => PositionLeg::Spot,
                "swap" | "usdm_futures" => PositionLeg::Futures,
                _ => bail!("unsupported Binance intra market {:?}", row.market),
            };
            Ok(NormalizedTrade {
                symbol,
                side,
                leg,
                price: row.price,
                amount_u: row.amount_u,
                fee_usdt,
                ts: row.ts,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let funding = sqlx::query_as::<_, BinanceIntraFundingRow>(AssertSqlSafe(funding_sql))
        .bind(strategy_start_ms)
        .bind(end_ms)
        .fetch_all(pool)
        .await
        .with_context(|| format!("load Binance intra funding from {schema}"))?
        .into_iter()
        .map(|row| FundingEvent {
            symbol: row.symbol.to_ascii_uppercase(),
            amount_usdt: row.amount_usdt,
            ts: row.ts,
        })
        .collect();

    Ok(PnlInputs {
        trades,
        funding,
        interest: Vec::new(),
    })
}

fn binance_intra_fee_usdt(
    market: &str,
    liquidity_role: &str,
    amount_u: f64,
    observed_fee_usdt: Option<f64>,
) -> Option<f64> {
    if market.eq_ignore_ascii_case("spot") && liquidity_role.eq_ignore_ascii_case("maker") {
        Some(-amount_u * BINANCE_INTRA_SPOT_MAKER_REBATE_RATE)
    } else {
        observed_fee_usdt
    }
}

fn binance_fee_cost_usdt(
    fee: f64,
    commission_asset: &str,
    trade_price: f64,
    symbol: &str,
    mark_prices: &MarkPriceCache,
) -> Option<f64> {
    if !fee.is_finite() {
        return None;
    }
    let commission_asset = commission_asset.to_ascii_uppercase();
    if STABLECOINS.contains(&commission_asset.as_str()) {
        return Some(fee);
    }
    let base_asset = symbol.strip_suffix("USDT").unwrap_or("");
    if !base_asset.is_empty() && commission_asset == base_asset {
        return (trade_price.is_finite() && trade_price > 0.0).then_some(fee * trade_price);
    }
    mark_prices
        .price(
            MarkPriceExchange::Binance,
            &format!("{commission_asset}USDT"),
        )
        .map(|price| fee * price)
}

async fn load_market_making_inputs(
    pool: &PgPool,
    schema: &str,
    exchange: &str,
    strategy_start_ms: i64,
    end_ms: i64,
) -> Result<PnlInputs> {
    let trade_sql = format!(
        r#"SELECT symbol, side, price::float8 AS price,
                  quantity::float8 AS quantity,
                  quote_quantity::float8 AS quote_quantity,
                  fee_usdt::float8 AS fee_usdt, event_time_ms AS ts
           FROM {schema}.trades
           WHERE event_time_ms >= $1 AND event_time_ms <= $2
           ORDER BY event_time_ms, symbol, trade_id"#
    );
    let funding_sql = format!(
        r#"SELECT symbol, COALESCE(amount_usdt, amount)::float8 AS amount_usdt,
                  event_time_ms AS ts
           FROM {schema}.funding
           WHERE symbol IS NOT NULL
             AND event_time_ms >= $1 AND event_time_ms <= $2
           ORDER BY event_time_ms, symbol, record_id"#
    );

    let rows = sqlx::query_as::<_, MarketMakingTradeRow>(AssertSqlSafe(trade_sql))
        .bind(strategy_start_ms)
        .bind(end_ms)
        .fetch_all(pool)
        .await
        .with_context(|| format!("load market-making trades from {schema}"))?;
    let multiplier_book = if matches!(exchange, "gate" | "okx") && !rows.is_empty() {
        Some(ContractMultiplierBook::load(pool, exchange).await?)
    } else {
        None
    };
    let trades = rows
        .into_iter()
        .map(|row| {
            let side = match row.side.to_ascii_lowercase().as_str() {
                "buy" => Side::Buy,
                "sell" => Side::Sell,
                _ => bail!("unsupported market-making trade side {:?}", row.side),
            };
            let symbol = row.symbol.to_ascii_uppercase();
            let multiplier = multiplier_book
                .as_ref()
                .map(|book| book.multiplier_at(&symbol, row.ts))
                .transpose()?;
            let amount_u = market_making_amount_u(
                exchange,
                row.price,
                row.quantity,
                row.quote_quantity,
                multiplier,
            )?;
            Ok(NormalizedTrade {
                symbol,
                side,
                leg: PositionLeg::Futures,
                price: row.price,
                amount_u,
                fee_usdt: row.fee_usdt,
                ts: row.ts,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let funding = sqlx::query_as::<_, MarketMakingFundingRow>(AssertSqlSafe(funding_sql))
        .bind(strategy_start_ms)
        .bind(end_ms)
        .fetch_all(pool)
        .await
        .with_context(|| format!("load market-making funding from {schema}"))?
        .into_iter()
        .map(|row| FundingEvent {
            symbol: row.symbol.to_ascii_uppercase(),
            amount_usdt: row.amount_usdt,
            ts: row.ts,
        })
        .collect();

    Ok(PnlInputs {
        trades,
        funding,
        interest: Vec::new(),
    })
}

fn market_making_amount_u(
    exchange: &str,
    price: f64,
    quantity: f64,
    quote_quantity: Option<f64>,
    contract_multiplier: Option<f64>,
) -> Result<f64> {
    let amount_u = match exchange {
        "gate" | "okx" => {
            let multiplier = contract_multiplier
                .with_context(|| format!("missing {exchange} contract multiplier"))?;
            price * quantity.abs() * multiplier
        }
        "binance" | "bybit" => quote_quantity
            .filter(|amount| *amount != 0.0)
            .unwrap_or(price * quantity),
        _ => bail!("unsupported market-making exchange {exchange}"),
    };
    if !amount_u.is_finite() || amount_u <= 0.0 {
        bail!("invalid {exchange} market-making amount_u: {amount_u}");
    }
    Ok(amount_u)
}

async fn load_spot_swap_inputs(
    pool: &PgPool,
    schema: &str,
    exchange: &str,
    strategy_start_ms: i64,
    end_ms: i64,
) -> Result<PnlInputs> {
    let (spot_key, futures_key) = match exchange {
        "bybit" => ("bybitspot", "bybitswap"),
        "gate" => ("gatespot", "gateswap"),
        _ => bail!("unsupported spot/swap PnL exchange {exchange}"),
    };
    let trade_sql = format!(
        r#"SELECT key, symbol, side, price::float8 AS price,
                  amountu::float8 AS amount_u, fees::float8 AS fee,
                  "commissionAsset" AS commission_asset, ts
           FROM {schema}.trades
           WHERE ts >= $1 AND ts <= $2
           ORDER BY ts, symbol, id"#
    );
    let funding_sql = format!(
        r#"SELECT symbol, funding::float8 AS funding,
                  "transactionTime" AS ts
           FROM {schema}.funding
           WHERE "transactionTime" >= $1 AND "transactionTime" <= $2
           ORDER BY "transactionTime", symbol, id"#
    );
    let interest_sql = format!(
        r#"SELECT currency, interest::float8 AS interest, "transactionTime" AS ts
           FROM {schema}.interest
           WHERE "transactionTime" >= $1 AND "transactionTime" <= $2
           ORDER BY "transactionTime", currency, id"#
    );

    let rows = sqlx::query_as::<_, SpotSwapTradeRow>(AssertSqlSafe(trade_sql))
        .bind(strategy_start_ms)
        .bind(end_ms)
        .fetch_all(pool)
        .await
        .with_context(|| format!("load {exchange} trades from {schema}"))?;
    let mut trades = Vec::with_capacity(rows.len());
    let mut last_spot_prices = HashMap::new();
    for row in rows {
        let side = match row.side.as_str() {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => bail!("unsupported trade side {:?}", row.side),
        };
        let symbol = row.symbol.to_ascii_uppercase();
        if row.key == spot_key {
            last_spot_prices.insert(symbol.clone(), row.price);
        }
        let commission_asset = row.commission_asset.to_ascii_uppercase();
        let base_asset = symbol.strip_suffix("USDT").unwrap_or("");
        let fee_usdt = if STABLECOINS.contains(&commission_asset.as_str()) {
            Some(row.fee)
        } else if !base_asset.is_empty() && commission_asset == base_asset {
            Some(row.fee * row.price)
        } else {
            None
        };
        let leg = match row.key.as_str() {
            key if key == spot_key => PositionLeg::Spot,
            key if key == futures_key => PositionLeg::Futures,
            _ => bail!("unsupported {exchange} trade key {:?}", row.key),
        };
        trades.push(NormalizedTrade {
            symbol,
            side,
            leg,
            price: row.price,
            amount_u: row.amount_u,
            fee_usdt,
            ts: row.ts,
        });
    }

    let funding = sqlx::query_as::<_, SpotSwapFundingRow>(AssertSqlSafe(funding_sql))
        .bind(strategy_start_ms)
        .bind(end_ms)
        .fetch_all(pool)
        .await
        .with_context(|| format!("load {exchange} funding from {schema}"))?
        .into_iter()
        .map(|row| FundingEvent {
            symbol: row.symbol.to_ascii_uppercase(),
            amount_usdt: row.funding,
            ts: row.ts,
        })
        .collect();

    let interest = sqlx::query_as::<_, SpotSwapInterestRow>(AssertSqlSafe(interest_sql))
        .bind(strategy_start_ms)
        .bind(end_ms)
        .fetch_all(pool)
        .await
        .with_context(|| format!("load {exchange} interest from {schema}"))?
        .into_iter()
        .map(|row| {
            let currency = row.currency.to_ascii_uppercase();
            let symbol = if STABLECOINS.contains(&currency.as_str()) {
                currency.clone()
            } else {
                format!("{currency}USDT")
            };
            let conversion_price = if STABLECOINS.contains(&currency.as_str()) {
                Some(1.0)
            } else {
                last_spot_prices.get(&symbol).copied()
            };
            InterestEvent {
                symbol,
                cost_usdt: conversion_price.map(|price| (-row.interest * price).max(0.0)),
                ts: row.ts,
            }
        })
        .collect();

    Ok(PnlInputs {
        trades,
        funding,
        interest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trade(
        symbol: &str,
        leg: PositionLeg,
        side: Side,
        price: f64,
        amount_u: f64,
        fee: f64,
        ts: i64,
    ) -> NormalizedTrade {
        NormalizedTrade {
            symbol: symbol.to_string(),
            side,
            leg,
            price,
            amount_u,
            fee_usdt: Some(fee),
            ts,
        }
    }

    fn request(start_ms: i64, end_ms: i64) -> PnlCalculation {
        PnlCalculation {
            source: PnlSourceKind::Intra,
            exchange: "bybit".to_string(),
            strategy_start_ms: 1_000,
            start_ms,
            end_ms,
            selected_symbols: Vec::new(),
            max_points: 1_000,
        }
    }

    #[test]
    fn keeps_venue_fifo_independent_and_combines_fees_funding_and_interest() {
        let inputs = PnlInputs {
            trades: vec![
                trade(
                    "BTCUSDT",
                    PositionLeg::Spot,
                    Side::Buy,
                    100.0,
                    1_000.0,
                    2.0,
                    1_100,
                ),
                trade(
                    "BTCUSDT",
                    PositionLeg::Futures,
                    Side::Sell,
                    110.0,
                    400.0,
                    1.0,
                    1_200,
                ),
            ],
            funding: vec![FundingEvent {
                symbol: "BTCUSDT".to_string(),
                amount_usdt: 5.0,
                ts: 1_300,
            }],
            interest: vec![InterestEvent {
                symbol: "BTCUSDT".to_string(),
                cost_usdt: Some(2.0),
                ts: 1_350,
            }],
        };

        let response = calculate(inputs, request(1_000, 1_400)).unwrap();

        assert_eq!(response.summary.fee_before_pnl_usdt, 0.0);
        assert!((response.summary.trading_fee_usdt - 3.0).abs() < 1e-9);
        assert!((response.summary.fee_after_pnl_usdt + 3.0).abs() < 1e-9);
        assert!((response.summary.funding_pnl_usdt - 5.0).abs() < 1e-9);
        assert!((response.summary.interest_cost_usdt - 2.0).abs() < 1e-9);
        assert_eq!(response.summary.floating_pnl_usdt, 0.0);
        assert_eq!(response.summary.total_pnl_usdt, 0.0);
        let final_point = response.points.last().unwrap();
        assert_eq!(final_point.spot_position_usdt, 1_000.0);
        assert_eq!(final_point.futures_position_usdt, -400.0);
        assert_eq!(final_point.exposure_usdt, 600.0);
        assert_eq!(response.symbol_points.len(), 1);
        let symbol_series = &response.symbol_points[0];
        assert_eq!(symbol_series.symbol, "BTCUSDT");
        assert_eq!(symbol_series.points.first().unwrap().total_pnl_usdt, 0.0);
        assert_eq!(
            symbol_series.points.last().unwrap().total_pnl_usdt,
            response.symbols[0].pnl.total_pnl_usdt
        );
        assert_eq!(
            response.source.returned_symbol_points,
            symbol_series.points.len()
        );
    }

    #[test]
    fn closes_fifo_only_within_the_same_venue() {
        let inputs = PnlInputs {
            trades: vec![
                trade(
                    "BTCUSDT",
                    PositionLeg::Spot,
                    Side::Buy,
                    100.0,
                    1_000.0,
                    0.0,
                    1_100,
                ),
                trade(
                    "BTCUSDT",
                    PositionLeg::Futures,
                    Side::Sell,
                    110.0,
                    400.0,
                    0.0,
                    1_200,
                ),
                trade(
                    "BTCUSDT",
                    PositionLeg::Spot,
                    Side::Sell,
                    105.0,
                    1_000.0,
                    0.0,
                    1_300,
                ),
                trade(
                    "BTCUSDT",
                    PositionLeg::Futures,
                    Side::Buy,
                    99.0,
                    400.0,
                    0.0,
                    1_400,
                ),
            ],
            ..PnlInputs::default()
        };

        let response = calculate(inputs, request(1_000, 1_500)).unwrap();

        assert!((response.summary.fee_before_pnl_usdt - 90.0).abs() < 1e-9);
        assert!((response.summary.total_pnl_usdt - 90.0).abs() < 1e-9);
        assert_eq!(response.summary.open_amount_usdt, 0.0);
        let final_point = response.points.last().unwrap();
        assert_eq!(final_point.spot_position_usdt, 0.0);
        assert_eq!(final_point.futures_position_usdt, 0.0);
    }

    #[test]
    fn market_making_exposure_is_signed_venue_position_sum() {
        let inputs = PnlInputs {
            trades: vec![
                NormalizedTrade {
                    symbol: "BTCUSDT".to_string(),
                    side: Side::Sell,
                    leg: PositionLeg::Futures,
                    price: 100.0,
                    amount_u: 500.0,
                    fee_usdt: Some(0.0),
                    ts: 1_100,
                },
                NormalizedTrade {
                    symbol: "ETHUSDT".to_string(),
                    side: Side::Buy,
                    leg: PositionLeg::Futures,
                    price: 10.0,
                    amount_u: 300.0,
                    fee_usdt: Some(0.0),
                    ts: 1_200,
                },
            ],
            ..PnlInputs::default()
        };
        let mut calculation = request(1_000, 1_300);
        calculation.source = PnlSourceKind::MarketMaking;

        let response = calculate(inputs, calculation).unwrap();
        let final_point = response.points.last().unwrap();

        assert_eq!(final_point.spot_position_usdt, 0.0);
        assert_eq!(final_point.futures_position_usdt, -200.0);
        assert_eq!(final_point.exposure_usdt, -200.0);
        assert_eq!(response.summary.open_amount_usdt, -200.0);
        assert!(!response.source.interest_included);
        assert_eq!(response.source.adapter, "market_making_futures_v1");
    }

    #[test]
    fn gate_and_okx_market_making_amount_uses_contract_multiplier() {
        assert_eq!(
            market_making_amount_u("gate", 100.0, 5.0, Some(9_999.0), Some(0.01)).unwrap(),
            5.0
        );
        assert_eq!(
            market_making_amount_u("okx", 25.0, -4.0, None, Some(0.1)).unwrap(),
            10.0
        );
        assert!(market_making_amount_u("gate", 100.0, 5.0, None, None).is_err());
    }

    #[test]
    fn binance_and_bybit_market_making_amount_does_not_use_multiplier() {
        assert_eq!(
            market_making_amount_u("binance", 100.0, 5.0, Some(42.0), Some(99.0)).unwrap(),
            42.0
        );
        assert_eq!(
            market_making_amount_u("bybit", 100.0, 5.0, None, Some(99.0)).unwrap(),
            500.0
        );
    }

    #[test]
    fn binance_intra_spot_maker_uses_fixed_rebate_patch() {
        assert_eq!(
            binance_intra_fee_usdt("spot", "maker", 2_500.0, Some(0.0)),
            Some(-0.1)
        );
        assert_eq!(
            binance_intra_fee_usdt("spot", "taker", 2_500.0, Some(0.25)),
            Some(0.25)
        );
        assert_eq!(
            binance_intra_fee_usdt("swap", "maker", 2_500.0, Some(0.05)),
            Some(0.05)
        );
    }

    #[test]
    fn matches_only_supported_strategy_and_exchange_pairs() {
        assert_eq!(
            PnlSourceKind::for_strategy("intra_exchange", "binance", "usdm_futures"),
            Some(PnlSourceKind::Intra)
        );
        assert_eq!(
            PnlSourceKind::for_strategy("intra_exchange", "bybit", "unified"),
            Some(PnlSourceKind::Intra)
        );
        assert_eq!(
            PnlSourceKind::for_strategy("intra_exchange", "gate", "unified"),
            Some(PnlSourceKind::Intra)
        );
        assert_eq!(
            PnlSourceKind::for_strategy("funding_rate", "gate", "unified"),
            Some(PnlSourceKind::FundingRate)
        );
        assert_eq!(PnlSourceKind::for_strategy("cta", "bybit", "unified"), None);
    }

    #[test]
    fn spot_swap_source_metadata_depends_only_on_exchange_capability() {
        assert_eq!(PnlSourceKind::Intra.adapter_name(), "spot_swap_history_v1");
        assert_eq!(
            PnlSourceKind::FundingRate.adapter_name(),
            "spot_swap_history_v1"
        );
        assert!(!PnlSourceKind::Intra.interest_included("binance"));
        assert!(PnlSourceKind::Intra.interest_included("bybit"));
        assert!(PnlSourceKind::FundingRate.interest_included("gate"));
        assert_eq!(PnlSourceKind::FundingRate.exposure(1_000.0, -980.0), 20.0);
    }

    #[test]
    fn binance_fee_uses_stable_base_or_cached_mark_price() {
        let prices = MarkPriceCache::default();
        prices.update(MarkPriceExchange::Binance, "BNBUSDT", 600.0, 1);

        assert_eq!(
            binance_fee_cost_usdt(1.5, "USDT", 100.0, "BTCUSDT", &prices),
            Some(1.5)
        );
        assert_eq!(
            binance_fee_cost_usdt(0.01, "BTC", 100.0, "BTCUSDT", &prices),
            Some(1.0)
        );
        assert_eq!(
            binance_fee_cost_usdt(0.002, "BNB", 100.0, "BTCUSDT", &prices),
            Some(1.2)
        );
        assert_eq!(
            binance_fee_cost_usdt(0.1, "UNKNOWN", 100.0, "BTCUSDT", &prices),
            None
        );
    }
}
