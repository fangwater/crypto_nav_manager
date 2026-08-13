//! Research-only combination FIFO for synthesized intra maker/taker orders.

use anyhow::{Result, bail};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

const QUANTITY_EPSILON: f64 = 1e-10;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArbDirection {
    Positive,
    Reverse,
}

impl ArbDirection {
    pub fn from_margin_side(side: &str) -> Result<Self> {
        match side.to_ascii_lowercase().as_str() {
            "buy" => Ok(Self::Positive),
            "sell" => Ok(Self::Reverse),
            _ => bail!("unsupported intra analysis side {side:?}"),
        }
    }

    fn sign(self) -> f64 {
        match self {
            Self::Positive => 1.0,
            Self::Reverse => -1.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct IntraAnalysisOrder {
    pub fkey: i64,
    pub symbol: String,
    pub direction: ArbDirection,
    pub completed_at_ms: i64,
    pub spot_price: f64,
    pub futures_price: f64,
    pub quantity: f64,
    pub premium: Option<PremiumIndexCandle>,
}

#[derive(Clone, Debug)]
pub struct IntraAnalysisFeeEvent {
    pub symbol: String,
    pub ts: i64,
    pub notional_usdt: f64,
    /// Positive values are costs and negative values are rebates.
    pub fee_usdt: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct IntraAnalysisFundingEvent {
    pub symbol: String,
    pub ts: i64,
    /// Account cash flow: positive values are income and negative values are costs.
    pub amount_usdt: f64,
}

impl IntraAnalysisOrder {
    fn basis(&self) -> f64 {
        self.futures_price - self.spot_price
    }

    fn basis_bps(&self) -> f64 {
        self.basis() / self.spot_price * 10_000.0
    }

    fn market_basis(&self) -> Option<f64> {
        self.premium
            .map(|premium| self.spot_price * premium.close_rate)
    }

    fn execution_edge_bps(&self) -> Option<f64> {
        self.premium
            .map(|premium| self.basis_bps() - premium.close_rate * 10_000.0)
    }

    fn execution_capture_usdt(&self) -> Option<f64> {
        self.market_basis().map(|market_basis| {
            self.quantity * self.direction.sign() * (self.basis() - market_basis)
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PremiumIndexCandle {
    pub open_rate: f64,
    pub high_rate: f64,
    pub low_rate: f64,
    pub close_rate: f64,
}

#[derive(Clone, Debug)]
pub struct IntraAnalysisRequest {
    pub strategy_slug: String,
    pub display_name: String,
    pub premium_adapter: &'static str,
    pub strategy_start_ms: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub selected_symbols: Vec<String>,
    pub reference_fee_bps: f64,
    pub max_points: usize,
    pub max_matches: usize,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntraAnalysisResponse {
    pub strategy_slug: String,
    pub display_name: String,
    pub strategy_start_ms: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub selected_symbols: Vec<String>,
    pub available_symbols: Vec<String>,
    pub summary: IntraAnalysisSummary,
    pub symbols: Vec<IntraSymbolAnalysis>,
    pub points: Vec<IntraAnalysisPoint>,
    pub symbol_points: Vec<IntraSymbolSeries>,
    pub matches: Vec<IntraClosedMatch>,
    pub source: IntraAnalysisSource,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntraAnalysisSummary {
    pub mt_count: u64,
    pub positive_mt_count: u64,
    pub reverse_mt_count: u64,
    pub closed_match_count: u64,
    pub winning_match_count: u64,
    pub win_rate: f64,
    pub mt_notional_usdt: f64,
    pub matched_quantity: f64,
    pub matched_notional_usdt: f64,
    pub realized_pnl_usdt: f64,
    pub return_bps: f64,
    pub funding_pnl_usdt: f64,
    pub funding_return_bps: f64,
    pub gross_pnl_usdt: f64,
    pub gross_return_bps: f64,
    pub trading_fee_usdt: f64,
    pub fee_after_pnl_usdt: f64,
    pub fee_after_return_bps: f64,
    pub reference_fee_bps: f64,
    pub reference_trading_fee_usdt: f64,
    pub reference_fee_after_pnl_usdt: f64,
    pub reference_fee_after_return_bps: f64,
    pub fee_trade_count: u64,
    pub fee_trade_notional_usdt: f64,
    pub converted_fee_trade_count: u64,
    pub actual_fee_coverage: f64,
    pub decomposed_match_count: u64,
    pub premium_coverage: f64,
    pub decomposed_notional_usdt: f64,
    pub market_pnl_usdt: f64,
    pub execution_pnl_usdt: f64,
    pub market_return_bps: f64,
    pub execution_return_bps: f64,
    pub execution_mt_count: u64,
    pub execution_mt_notional_usdt: f64,
    pub execution_mt_premium_coverage: f64,
    pub execution_capture_usdt: f64,
    pub execution_capture_return_bps: f64,
    pub positive_execution_capture_usdt: f64,
    pub reverse_execution_capture_usdt: f64,
    pub average_holding_ms: f64,
    pub positive_open_lot_count: u64,
    pub reverse_open_lot_count: u64,
    pub positive_open_quantity: f64,
    pub reverse_open_quantity: f64,
    pub positive_open_notional_usdt: f64,
    pub reverse_open_notional_usdt: f64,
    pub positive_average_basis_bps: f64,
    pub reverse_average_basis_bps: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntraSymbolAnalysis {
    pub symbol: String,
    #[serde(flatten)]
    pub summary: IntraAnalysisSummary,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntraAnalysisPoint {
    pub ts: i64,
    pub realized_pnl_usdt: f64,
    pub funding_pnl_usdt: f64,
    pub gross_pnl_usdt: f64,
    pub trading_fee_usdt: f64,
    pub fee_after_pnl_usdt: f64,
    pub reference_trading_fee_usdt: f64,
    pub reference_fee_after_pnl_usdt: f64,
    pub market_pnl_usdt: f64,
    pub execution_pnl_usdt: f64,
    pub execution_capture_usdt: f64,
    pub matched_notional_usdt: f64,
    pub closed_match_count: u64,
    pub decomposed_match_count: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntraSymbolSeries {
    pub symbol: String,
    pub points: Vec<IntraAnalysisPoint>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntraClosedMatch {
    pub symbol: String,
    pub open_direction: ArbDirection,
    pub open_fkey: i64,
    pub close_fkey: i64,
    pub opened_at_ms: i64,
    pub closed_at_ms: i64,
    pub holding_ms: i64,
    pub quantity: f64,
    pub open_spot_price: f64,
    pub open_futures_price: f64,
    pub close_spot_price: f64,
    pub close_futures_price: f64,
    pub entry_basis_bps: f64,
    pub exit_basis_bps: f64,
    pub entry_premium_bps: Option<f64>,
    pub exit_premium_bps: Option<f64>,
    pub entry_execution_edge_bps: Option<f64>,
    pub exit_execution_edge_bps: Option<f64>,
    pub market_pnl_usdt: Option<f64>,
    pub entry_execution_pnl_usdt: Option<f64>,
    pub exit_execution_pnl_usdt: Option<f64>,
    pub execution_pnl_usdt: Option<f64>,
    pub funding_pnl_usdt: f64,
    pub gross_pnl_usdt: f64,
    pub fee_notional_usdt: f64,
    pub trading_fee_usdt: f64,
    pub reference_trading_fee_usdt: f64,
    pub fee_after_pnl_usdt: f64,
    pub reference_fee_after_pnl_usdt: f64,
    pub pnl_usdt: f64,
    pub return_bps: f64,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntraAnalysisSource {
    pub adapter: &'static str,
    pub hedge_price_adapter: &'static str,
    pub premium_adapter: &'static str,
    pub premium_rate_field: &'static str,
    pub loaded_mt_rows: usize,
    pub window_mt_rows: usize,
    pub loaded_fee_trade_rows: usize,
    pub window_fee_trade_rows: usize,
    pub converted_fee_trade_rows: usize,
    pub fee_allocation: &'static str,
    pub loaded_funding_rows: usize,
    pub window_funding_rows: usize,
    pub allocated_funding_rows: usize,
    pub funding_allocation: &'static str,
    pub returned_points: usize,
    pub returned_symbol_points: usize,
    pub returned_matches: usize,
    pub sampled: bool,
    pub fees_included: bool,
    pub funding_included: bool,
}

#[derive(Clone, Copy, Debug, Default)]
struct FeeAccumulator {
    trade_count: u64,
    converted_trade_count: u64,
    trade_notional_usdt: f64,
    converted_notional_usdt: f64,
    trading_fee_usdt: f64,
}

impl FeeAccumulator {
    fn record(&mut self, event: &IntraAnalysisFeeEvent) {
        self.trade_count += 1;
        self.trade_notional_usdt += event.notional_usdt;
        if let Some(fee_usdt) = event.fee_usdt {
            self.converted_trade_count += 1;
            self.converted_notional_usdt += event.notional_usdt;
            self.trading_fee_usdt += fee_usdt;
        }
    }

    fn allocated_actual_fee(self, fee_notional_usdt: f64) -> f64 {
        fee_notional_usdt * ratio(self.trading_fee_usdt, self.trade_notional_usdt)
    }

    fn apply_metadata_to_summary(self, summary: &mut IntraAnalysisSummary, reference_fee_bps: f64) {
        summary.reference_fee_bps = reference_fee_bps;
        summary.fee_trade_count = self.trade_count;
        summary.fee_trade_notional_usdt = clean_zero(self.trade_notional_usdt);
        summary.converted_fee_trade_count = self.converted_trade_count;
        summary.actual_fee_coverage = ratio(self.converted_notional_usdt, self.trade_notional_usdt);
    }
}

#[derive(Clone, Debug)]
struct OpenLot {
    fkey: i64,
    direction: ArbDirection,
    completed_at_ms: i64,
    spot_price: f64,
    futures_price: f64,
    remaining_quantity: f64,
    funding_pnl_usdt: f64,
    premium: Option<PremiumIndexCandle>,
}

impl OpenLot {
    fn basis(&self) -> f64 {
        self.futures_price - self.spot_price
    }

    fn basis_bps(&self) -> f64 {
        self.basis() / self.spot_price * 10_000.0
    }

    fn market_basis(&self) -> Option<f64> {
        self.premium
            .map(|premium| self.spot_price * premium.close_rate)
    }

    fn execution_edge_bps(&self) -> Option<f64> {
        self.premium
            .map(|premium| self.basis_bps() - premium.close_rate * 10_000.0)
    }

    fn execution_capture_usdt(&self, quantity: f64) -> Option<f64> {
        self.market_basis()
            .map(|market_basis| quantity * self.direction.sign() * (self.basis() - market_basis))
    }
}

#[derive(Clone, Debug, Default)]
struct SymbolState {
    positive: VecDeque<OpenLot>,
    reverse: VecDeque<OpenLot>,
    stats: SummaryAccumulator,
}

#[derive(Clone, Copy, Debug, Default)]
struct SummaryAccumulator {
    mt_count: u64,
    positive_mt_count: u64,
    reverse_mt_count: u64,
    closed_match_count: u64,
    winning_match_count: u64,
    mt_notional_usdt: f64,
    matched_quantity: f64,
    matched_notional_usdt: f64,
    realized_pnl_usdt: f64,
    funding_pnl_usdt: f64,
    trading_fee_usdt: f64,
    reference_trading_fee_usdt: f64,
    decomposed_match_count: u64,
    decomposed_notional_usdt: f64,
    market_pnl_usdt: f64,
    execution_pnl_usdt: f64,
    execution_mt_count: u64,
    execution_mt_notional_usdt: f64,
    execution_capture_usdt: f64,
    positive_execution_capture_usdt: f64,
    reverse_execution_capture_usdt: f64,
    holding_notional_ms: f64,
}

impl SummaryAccumulator {
    fn record_order(&mut self, order: &IntraAnalysisOrder) {
        self.mt_count += 1;
        match order.direction {
            ArbDirection::Positive => self.positive_mt_count += 1,
            ArbDirection::Reverse => self.reverse_mt_count += 1,
        }
        self.mt_notional_usdt += order.quantity * order.spot_price;
        if let Some(execution_capture) = order.execution_capture_usdt() {
            self.execution_mt_count += 1;
            self.execution_mt_notional_usdt += order.quantity * order.spot_price;
            self.execution_capture_usdt += execution_capture;
            match order.direction {
                ArbDirection::Positive => self.positive_execution_capture_usdt += execution_capture,
                ArbDirection::Reverse => self.reverse_execution_capture_usdt += execution_capture,
            }
        }
    }

    fn record_match(&mut self, row: &IntraClosedMatch, matched_notional: f64) {
        self.closed_match_count += 1;
        if row.pnl_usdt > 0.0 {
            self.winning_match_count += 1;
        }
        self.matched_quantity += row.quantity;
        self.matched_notional_usdt += matched_notional;
        self.realized_pnl_usdt += row.pnl_usdt;
        self.funding_pnl_usdt += row.funding_pnl_usdt;
        self.trading_fee_usdt += row.trading_fee_usdt;
        self.reference_trading_fee_usdt += row.reference_trading_fee_usdt;
        if let (Some(market_pnl), Some(execution_pnl)) =
            (row.market_pnl_usdt, row.execution_pnl_usdt)
        {
            self.decomposed_match_count += 1;
            self.decomposed_notional_usdt += matched_notional;
            self.market_pnl_usdt += market_pnl;
            self.execution_pnl_usdt += execution_pnl;
        }
        self.holding_notional_ms += row.holding_ms as f64 * matched_notional;
    }

    fn add(&mut self, other: Self) {
        self.mt_count += other.mt_count;
        self.positive_mt_count += other.positive_mt_count;
        self.reverse_mt_count += other.reverse_mt_count;
        self.closed_match_count += other.closed_match_count;
        self.winning_match_count += other.winning_match_count;
        self.mt_notional_usdt += other.mt_notional_usdt;
        self.matched_quantity += other.matched_quantity;
        self.matched_notional_usdt += other.matched_notional_usdt;
        self.realized_pnl_usdt += other.realized_pnl_usdt;
        self.funding_pnl_usdt += other.funding_pnl_usdt;
        self.trading_fee_usdt += other.trading_fee_usdt;
        self.reference_trading_fee_usdt += other.reference_trading_fee_usdt;
        self.decomposed_match_count += other.decomposed_match_count;
        self.decomposed_notional_usdt += other.decomposed_notional_usdt;
        self.market_pnl_usdt += other.market_pnl_usdt;
        self.execution_pnl_usdt += other.execution_pnl_usdt;
        self.execution_mt_count += other.execution_mt_count;
        self.execution_mt_notional_usdt += other.execution_mt_notional_usdt;
        self.execution_capture_usdt += other.execution_capture_usdt;
        self.positive_execution_capture_usdt += other.positive_execution_capture_usdt;
        self.reverse_execution_capture_usdt += other.reverse_execution_capture_usdt;
        self.holding_notional_ms += other.holding_notional_ms;
    }

    fn finish(self, open: OpenSummary) -> IntraAnalysisSummary {
        let gross_pnl_usdt = self.realized_pnl_usdt + self.funding_pnl_usdt;
        IntraAnalysisSummary {
            mt_count: self.mt_count,
            positive_mt_count: self.positive_mt_count,
            reverse_mt_count: self.reverse_mt_count,
            closed_match_count: self.closed_match_count,
            winning_match_count: self.winning_match_count,
            win_rate: ratio(
                self.winning_match_count as f64,
                self.closed_match_count as f64,
            ),
            mt_notional_usdt: clean_zero(self.mt_notional_usdt),
            matched_quantity: clean_zero(self.matched_quantity),
            matched_notional_usdt: clean_zero(self.matched_notional_usdt),
            realized_pnl_usdt: clean_zero(self.realized_pnl_usdt),
            return_bps: ratio(self.realized_pnl_usdt, self.matched_notional_usdt) * 10_000.0,
            funding_pnl_usdt: clean_zero(self.funding_pnl_usdt),
            funding_return_bps: ratio(self.funding_pnl_usdt, self.matched_notional_usdt) * 10_000.0,
            gross_pnl_usdt: clean_zero(gross_pnl_usdt),
            gross_return_bps: ratio(gross_pnl_usdt, self.matched_notional_usdt) * 10_000.0,
            trading_fee_usdt: clean_zero(self.trading_fee_usdt),
            fee_after_pnl_usdt: clean_zero(gross_pnl_usdt - self.trading_fee_usdt),
            fee_after_return_bps: ratio(
                gross_pnl_usdt - self.trading_fee_usdt,
                self.matched_notional_usdt,
            ) * 10_000.0,
            reference_trading_fee_usdt: clean_zero(self.reference_trading_fee_usdt),
            reference_fee_after_pnl_usdt: clean_zero(
                gross_pnl_usdt - self.reference_trading_fee_usdt,
            ),
            reference_fee_after_return_bps: ratio(
                gross_pnl_usdt - self.reference_trading_fee_usdt,
                self.matched_notional_usdt,
            ) * 10_000.0,
            decomposed_match_count: self.decomposed_match_count,
            premium_coverage: ratio(
                self.decomposed_match_count as f64,
                self.closed_match_count as f64,
            ),
            decomposed_notional_usdt: clean_zero(self.decomposed_notional_usdt),
            market_pnl_usdt: clean_zero(self.market_pnl_usdt),
            execution_pnl_usdt: clean_zero(self.execution_pnl_usdt),
            market_return_bps: ratio(self.market_pnl_usdt, self.decomposed_notional_usdt)
                * 10_000.0,
            execution_return_bps: ratio(self.execution_pnl_usdt, self.decomposed_notional_usdt)
                * 10_000.0,
            execution_mt_count: self.execution_mt_count,
            execution_mt_notional_usdt: clean_zero(self.execution_mt_notional_usdt),
            execution_mt_premium_coverage: ratio(
                self.execution_mt_count as f64,
                self.mt_count as f64,
            ),
            execution_capture_usdt: clean_zero(self.execution_capture_usdt),
            execution_capture_return_bps: ratio(
                self.execution_capture_usdt,
                self.execution_mt_notional_usdt,
            ) * 10_000.0,
            positive_execution_capture_usdt: clean_zero(self.positive_execution_capture_usdt),
            reverse_execution_capture_usdt: clean_zero(self.reverse_execution_capture_usdt),
            average_holding_ms: ratio(self.holding_notional_ms, self.matched_notional_usdt),
            positive_open_lot_count: open.positive_lot_count,
            reverse_open_lot_count: open.reverse_lot_count,
            positive_open_quantity: clean_zero(open.positive_quantity),
            reverse_open_quantity: clean_zero(open.reverse_quantity),
            positive_open_notional_usdt: clean_zero(open.positive_notional_usdt),
            reverse_open_notional_usdt: clean_zero(open.reverse_notional_usdt),
            positive_average_basis_bps: ratio(
                open.positive_basis_notional_bps,
                open.positive_notional_usdt,
            ),
            reverse_average_basis_bps: ratio(
                open.reverse_basis_notional_bps,
                open.reverse_notional_usdt,
            ),
            ..IntraAnalysisSummary::default()
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct OpenSummary {
    positive_lot_count: u64,
    reverse_lot_count: u64,
    positive_quantity: f64,
    reverse_quantity: f64,
    positive_notional_usdt: f64,
    reverse_notional_usdt: f64,
    positive_basis_notional_bps: f64,
    reverse_basis_notional_bps: f64,
}

impl OpenSummary {
    fn from_state(state: &SymbolState) -> Self {
        let mut summary = Self::default();
        for lot in &state.positive {
            let notional = lot.remaining_quantity * lot.spot_price;
            summary.positive_lot_count += 1;
            summary.positive_quantity += lot.remaining_quantity;
            summary.positive_notional_usdt += notional;
            summary.positive_basis_notional_bps += lot.basis_bps() * notional;
        }
        for lot in &state.reverse {
            let notional = lot.remaining_quantity * lot.spot_price;
            summary.reverse_lot_count += 1;
            summary.reverse_quantity += lot.remaining_quantity;
            summary.reverse_notional_usdt += notional;
            summary.reverse_basis_notional_bps += lot.basis_bps() * notional;
        }
        summary
    }

    fn add(&mut self, other: Self) {
        self.positive_lot_count += other.positive_lot_count;
        self.reverse_lot_count += other.reverse_lot_count;
        self.positive_quantity += other.positive_quantity;
        self.reverse_quantity += other.reverse_quantity;
        self.positive_notional_usdt += other.positive_notional_usdt;
        self.reverse_notional_usdt += other.reverse_notional_usdt;
        self.positive_basis_notional_bps += other.positive_basis_notional_bps;
        self.reverse_basis_notional_bps += other.reverse_basis_notional_bps;
    }
}

pub fn calculate(
    orders: Vec<IntraAnalysisOrder>,
    request: IntraAnalysisRequest,
) -> Result<IntraAnalysisResponse> {
    calculate_with_fees_and_funding(orders, Vec::new(), Vec::new(), request)
}

pub fn calculate_with_fees(
    orders: Vec<IntraAnalysisOrder>,
    fee_events: Vec<IntraAnalysisFeeEvent>,
    request: IntraAnalysisRequest,
) -> Result<IntraAnalysisResponse> {
    calculate_with_fees_and_funding(orders, fee_events, Vec::new(), request)
}

pub fn calculate_with_fees_and_funding(
    mut orders: Vec<IntraAnalysisOrder>,
    mut fee_events: Vec<IntraAnalysisFeeEvent>,
    mut funding_events: Vec<IntraAnalysisFundingEvent>,
    request: IntraAnalysisRequest,
) -> Result<IntraAnalysisResponse> {
    if request.start_ms < request.strategy_start_ms {
        bail!("start_ms must not be earlier than strategy_start_ms");
    }
    if request.end_ms < request.start_ms {
        bail!("end_ms must be greater than or equal to start_ms");
    }
    if !request.reference_fee_bps.is_finite() || request.reference_fee_bps.abs() > 100.0 {
        bail!("reference_fee_bps must be finite and between -100 and 100");
    }
    for order in &mut orders {
        order.symbol = order.symbol.to_ascii_uppercase();
        validate_order(order)?;
    }
    orders.sort_by(|left, right| {
        left.completed_at_ms
            .cmp(&right.completed_at_ms)
            .then_with(|| left.fkey.cmp(&right.fkey))
    });
    for event in &mut fee_events {
        event.symbol = event.symbol.to_ascii_uppercase();
        validate_fee_event(event)?;
    }
    fee_events.sort_by(|left, right| {
        left.ts
            .cmp(&right.ts)
            .then_with(|| left.symbol.cmp(&right.symbol))
    });
    for event in &mut funding_events {
        event.symbol = event.symbol.to_ascii_uppercase();
        validate_funding_event(event)?;
    }
    funding_events.sort_by(|left, right| {
        left.ts
            .cmp(&right.ts)
            .then_with(|| left.symbol.cmp(&right.symbol))
    });

    let loaded_mt_rows = orders.len();
    let loaded_fee_trade_rows = fee_events.len();
    let loaded_funding_rows = funding_events.len();
    let available_symbols = orders
        .iter()
        .map(|order| order.symbol.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let selected_symbols = selected_symbols(&available_symbols, request.selected_symbols)?;
    let selected = selected_symbols.iter().cloned().collect::<HashSet<_>>();
    let mut aggregate_fees = FeeAccumulator::default();
    let mut symbol_fees = available_symbols
        .iter()
        .cloned()
        .map(|symbol| (symbol, FeeAccumulator::default()))
        .collect::<HashMap<_, _>>();
    let mut window_fee_trade_rows = 0_usize;
    let mut converted_fee_trade_rows = 0_usize;
    for event in fee_events
        .iter()
        .filter(|event| event.ts >= request.start_ms && event.ts <= request.end_ms)
    {
        symbol_fees
            .entry(event.symbol.clone())
            .or_default()
            .record(event);
        if selected.contains(&event.symbol) {
            window_fee_trade_rows += 1;
            converted_fee_trade_rows += usize::from(event.fee_usdt.is_some());
            aggregate_fees.record(event);
        }
    }
    let window_funding_events = funding_events
        .iter()
        .filter(|event| event.ts >= request.start_ms && event.ts <= request.end_ms)
        .collect::<Vec<_>>();
    let window_funding_rows = window_funding_events
        .iter()
        .filter(|event| selected.contains(&event.symbol))
        .count();
    let mut states = available_symbols
        .iter()
        .cloned()
        .map(|symbol| (symbol, SymbolState::default()))
        .collect::<HashMap<_, _>>();
    let mut points = vec![IntraAnalysisPoint {
        ts: request.start_ms,
        ..IntraAnalysisPoint::default()
    }];
    let mut point_pnl = 0.0;
    let mut point_funding_pnl = 0.0;
    let mut point_trading_fee = 0.0;
    let mut point_reference_trading_fee = 0.0;
    let mut point_market_pnl = 0.0;
    let mut point_execution_pnl = 0.0;
    let mut point_notional = 0.0;
    let mut point_matches = 0_u64;
    let mut point_decomposed_matches = 0_u64;
    let mut symbol_points = selected_symbols
        .iter()
        .cloned()
        .map(|symbol| {
            (
                symbol,
                vec![IntraAnalysisPoint {
                    ts: request.start_ms,
                    ..IntraAnalysisPoint::default()
                }],
            )
        })
        .collect::<HashMap<_, _>>();
    let mut symbol_point_totals = selected_symbols
        .iter()
        .cloned()
        .map(|symbol| (symbol, IntraAnalysisPoint::default()))
        .collect::<HashMap<_, _>>();
    let mut recent_matches = VecDeque::with_capacity(request.max_matches.saturating_add(1));
    let mut window_mt_rows = 0_usize;
    let mut allocated_funding_rows = 0_usize;
    let mut funding_index = 0_usize;

    for order in orders
        .iter()
        .filter(|order| order.completed_at_ms <= request.end_ms)
    {
        while let Some(event) = window_funding_events.get(funding_index) {
            if event.ts > order.completed_at_ms {
                break;
            }
            if states
                .get_mut(&event.symbol)
                .is_some_and(|state| allocate_funding(state, event.amount_usdt))
                && selected.contains(&event.symbol)
            {
                allocated_funding_rows += 1;
            }
            funding_index += 1;
        }
        let in_window = order.completed_at_ms >= request.start_ms;
        if in_window {
            window_mt_rows += 1;
        }
        let state = states
            .get_mut(&order.symbol)
            .expect("all order symbols have an initialized state");
        if in_window {
            state.stats.record_order(order);
        }

        let opposite = match order.direction {
            ArbDirection::Positive => &mut state.reverse,
            ArbDirection::Reverse => &mut state.positive,
        };
        let mut remaining = order.quantity;
        while remaining > QUANTITY_EPSILON {
            let Some(open) = opposite.front_mut() else {
                break;
            };
            let matched_quantity = remaining.min(open.remaining_quantity);
            let matched_notional = matched_quantity * (open.spot_price + order.spot_price) / 2.0;
            let pnl_usdt =
                matched_quantity * open.direction.sign() * (open.basis() - order.basis());
            let funding_pnl_usdt = if matched_quantity + QUANTITY_EPSILON >= open.remaining_quantity
            {
                open.funding_pnl_usdt
            } else {
                open.funding_pnl_usdt * matched_quantity / open.remaining_quantity
            };
            let gross_pnl_usdt = pnl_usdt + funding_pnl_usdt;
            let market_pnl_usdt = match (open.market_basis(), order.market_basis()) {
                (Some(open_market_basis), Some(close_market_basis)) => Some(
                    matched_quantity
                        * open.direction.sign()
                        * (open_market_basis - close_market_basis),
                ),
                _ => None,
            };
            let entry_execution_pnl_usdt = open.execution_capture_usdt(matched_quantity);
            let exit_execution_pnl_usdt = order.market_basis().map(|market_basis| {
                matched_quantity * order.direction.sign() * (order.basis() - market_basis)
            });
            let execution_pnl_usdt = match (entry_execution_pnl_usdt, exit_execution_pnl_usdt) {
                (Some(entry), Some(exit)) => Some(entry + exit),
                _ => None,
            };
            let fee_notional_usdt = matched_quantity
                * (open.spot_price + open.futures_price + order.spot_price + order.futures_price);
            let trading_fee_usdt = symbol_fees
                .get(&order.symbol)
                .copied()
                .unwrap_or_default()
                .allocated_actual_fee(fee_notional_usdt);
            let reference_trading_fee_usdt =
                fee_notional_usdt * request.reference_fee_bps / 10_000.0;
            let holding_ms = order.completed_at_ms.saturating_sub(open.completed_at_ms);
            let closed = IntraClosedMatch {
                symbol: order.symbol.clone(),
                open_direction: open.direction,
                open_fkey: open.fkey,
                close_fkey: order.fkey,
                opened_at_ms: open.completed_at_ms,
                closed_at_ms: order.completed_at_ms,
                holding_ms,
                quantity: matched_quantity,
                open_spot_price: open.spot_price,
                open_futures_price: open.futures_price,
                close_spot_price: order.spot_price,
                close_futures_price: order.futures_price,
                entry_basis_bps: open.basis_bps(),
                exit_basis_bps: order.basis_bps(),
                entry_premium_bps: open.premium.map(|premium| premium.close_rate * 10_000.0),
                exit_premium_bps: order.premium.map(|premium| premium.close_rate * 10_000.0),
                entry_execution_edge_bps: open.execution_edge_bps(),
                exit_execution_edge_bps: order.execution_edge_bps(),
                market_pnl_usdt,
                entry_execution_pnl_usdt,
                exit_execution_pnl_usdt,
                execution_pnl_usdt,
                funding_pnl_usdt: clean_zero(funding_pnl_usdt),
                gross_pnl_usdt: clean_zero(gross_pnl_usdt),
                fee_notional_usdt: clean_zero(fee_notional_usdt),
                trading_fee_usdt: clean_zero(trading_fee_usdt),
                reference_trading_fee_usdt: clean_zero(reference_trading_fee_usdt),
                fee_after_pnl_usdt: clean_zero(gross_pnl_usdt - trading_fee_usdt),
                reference_fee_after_pnl_usdt: clean_zero(
                    gross_pnl_usdt - reference_trading_fee_usdt,
                ),
                pnl_usdt,
                return_bps: ratio(pnl_usdt, matched_notional) * 10_000.0,
            };
            if in_window {
                state.stats.record_match(&closed, matched_notional);
                if selected.contains(&order.symbol) {
                    point_pnl += pnl_usdt;
                    point_funding_pnl += funding_pnl_usdt;
                    point_trading_fee += trading_fee_usdt;
                    point_reference_trading_fee += reference_trading_fee_usdt;
                    if let (Some(market_pnl), Some(execution_pnl)) =
                        (market_pnl_usdt, execution_pnl_usdt)
                    {
                        point_market_pnl += market_pnl;
                        point_execution_pnl += execution_pnl;
                        point_decomposed_matches += 1;
                    }
                    point_notional += matched_notional;
                    point_matches += 1;
                    let symbol_total = symbol_point_totals
                        .get_mut(&order.symbol)
                        .expect("selected symbols have point totals");
                    symbol_total.ts = order.completed_at_ms;
                    symbol_total.realized_pnl_usdt += pnl_usdt;
                    symbol_total.funding_pnl_usdt += funding_pnl_usdt;
                    symbol_total.gross_pnl_usdt =
                        symbol_total.realized_pnl_usdt + symbol_total.funding_pnl_usdt;
                    symbol_total.trading_fee_usdt += trading_fee_usdt;
                    symbol_total.fee_after_pnl_usdt =
                        symbol_total.gross_pnl_usdt - symbol_total.trading_fee_usdt;
                    symbol_total.reference_trading_fee_usdt += reference_trading_fee_usdt;
                    symbol_total.reference_fee_after_pnl_usdt =
                        symbol_total.gross_pnl_usdt - symbol_total.reference_trading_fee_usdt;
                    if let (Some(market_pnl), Some(execution_pnl)) =
                        (market_pnl_usdt, execution_pnl_usdt)
                    {
                        symbol_total.market_pnl_usdt += market_pnl;
                        symbol_total.execution_pnl_usdt += execution_pnl;
                        symbol_total.decomposed_match_count += 1;
                    }
                    symbol_total.matched_notional_usdt += matched_notional;
                    symbol_total.closed_match_count += 1;
                    push_or_replace_point(
                        symbol_points
                            .get_mut(&order.symbol)
                            .expect("selected symbols have point series"),
                        *symbol_total,
                    );
                    push_or_replace_point(
                        &mut points,
                        IntraAnalysisPoint {
                            ts: order.completed_at_ms,
                            realized_pnl_usdt: clean_zero(point_pnl),
                            funding_pnl_usdt: clean_zero(point_funding_pnl),
                            gross_pnl_usdt: clean_zero(point_pnl + point_funding_pnl),
                            trading_fee_usdt: clean_zero(point_trading_fee),
                            fee_after_pnl_usdt: clean_zero(
                                point_pnl + point_funding_pnl - point_trading_fee,
                            ),
                            reference_trading_fee_usdt: clean_zero(point_reference_trading_fee),
                            reference_fee_after_pnl_usdt: clean_zero(
                                point_pnl + point_funding_pnl - point_reference_trading_fee,
                            ),
                            market_pnl_usdt: clean_zero(point_market_pnl),
                            execution_pnl_usdt: clean_zero(point_execution_pnl),
                            matched_notional_usdt: clean_zero(point_notional),
                            closed_match_count: point_matches,
                            decomposed_match_count: point_decomposed_matches,
                            ..IntraAnalysisPoint::default()
                        },
                    );
                    if request.max_matches > 0 {
                        recent_matches.push_back(closed);
                        while recent_matches.len() > request.max_matches {
                            recent_matches.pop_front();
                        }
                    }
                }
            }
            remaining = clean_quantity(remaining - matched_quantity);
            open.funding_pnl_usdt = clean_zero(open.funding_pnl_usdt - funding_pnl_usdt);
            open.remaining_quantity = clean_quantity(open.remaining_quantity - matched_quantity);
            if open.remaining_quantity <= QUANTITY_EPSILON {
                opposite.pop_front();
            }
        }
        if remaining > QUANTITY_EPSILON {
            let lot = OpenLot {
                fkey: order.fkey,
                direction: order.direction,
                completed_at_ms: order.completed_at_ms,
                spot_price: order.spot_price,
                futures_price: order.futures_price,
                remaining_quantity: remaining,
                funding_pnl_usdt: 0.0,
                premium: order.premium,
            };
            match order.direction {
                ArbDirection::Positive => state.positive.push_back(lot),
                ArbDirection::Reverse => state.reverse.push_back(lot),
            }
        }
    }

    for event in &window_funding_events[funding_index..] {
        if states
            .get_mut(&event.symbol)
            .is_some_and(|state| allocate_funding(state, event.amount_usdt))
            && selected.contains(&event.symbol)
        {
            allocated_funding_rows += 1;
        }
    }

    ensure_final_point(&mut points, request.end_ms);
    for symbol in &selected_symbols {
        ensure_final_point(
            symbol_points
                .get_mut(symbol)
                .expect("selected symbols have point series"),
            request.end_ms,
        );
    }

    let mut aggregate_stats = SummaryAccumulator::default();
    let mut aggregate_open = OpenSummary::default();
    let mut symbols = available_symbols
        .iter()
        .map(|symbol| {
            let state = states
                .get(symbol)
                .expect("all available symbols retain an analysis state");
            let open = OpenSummary::from_state(state);
            if selected.contains(symbol) {
                aggregate_stats.add(state.stats);
                aggregate_open.add(open);
            }
            let mut summary = state.stats.finish(open);
            symbol_fees
                .get(symbol)
                .copied()
                .unwrap_or_default()
                .apply_metadata_to_summary(&mut summary, request.reference_fee_bps);
            IntraSymbolAnalysis {
                symbol: symbol.clone(),
                summary,
            }
        })
        .collect::<Vec<_>>();
    symbols.sort_by(|left, right| {
        right
            .summary
            .gross_pnl_usdt
            .total_cmp(&left.summary.gross_pnl_usdt)
            .then_with(|| left.symbol.cmp(&right.symbol))
    });

    let original_point_count = points.len();
    let points = downsample_points(points, request.max_points.max(2));
    let symbol_max_points = request.max_points.clamp(100, 800);
    let mut returned_symbol_points = 0;
    let mut sampled_symbol_points = false;
    let symbol_points = selected_symbols
        .iter()
        .map(|symbol| {
            let original = symbol_points.remove(symbol).unwrap_or_default();
            let original_len = original.len();
            let points = downsample_points(original, symbol_max_points);
            sampled_symbol_points |= points.len() < original_len;
            returned_symbol_points += points.len();
            IntraSymbolSeries {
                symbol: symbol.clone(),
                points,
            }
        })
        .collect();
    let mut matches = recent_matches.into_iter().collect::<Vec<_>>();
    matches.reverse();
    let source = IntraAnalysisSource {
        adapter: "intra_order_combo_fifo",
        hedge_price_adapter: "intra_hedges_main_fkey",
        premium_adapter: request.premium_adapter,
        premium_rate_field: "close",
        loaded_mt_rows,
        window_mt_rows,
        loaded_fee_trade_rows,
        window_fee_trade_rows,
        converted_fee_trade_rows,
        fee_allocation: "symbol_window_effective_rate_on_closed_four_legs",
        loaded_funding_rows,
        window_funding_rows,
        allocated_funding_rows,
        funding_allocation: "event_time_open_futures_notional_then_fifo_closed_quantity",
        returned_points: points.len(),
        returned_symbol_points,
        returned_matches: matches.len(),
        sampled: points.len() < original_point_count || sampled_symbol_points,
        fees_included: true,
        funding_included: true,
    };

    let mut summary = aggregate_stats.finish(aggregate_open);
    aggregate_fees.apply_metadata_to_summary(&mut summary, request.reference_fee_bps);
    Ok(IntraAnalysisResponse {
        strategy_slug: request.strategy_slug,
        display_name: request.display_name,
        strategy_start_ms: request.strategy_start_ms,
        start_ms: request.start_ms,
        end_ms: request.end_ms,
        selected_symbols,
        available_symbols,
        summary,
        symbols,
        points,
        symbol_points,
        matches,
        source,
    })
}

fn selected_symbols(available: &[String], requested: Vec<String>) -> Result<Vec<String>> {
    if requested.is_empty() {
        return Ok(available.to_vec());
    }
    let available = available.iter().cloned().collect::<HashSet<_>>();
    let selected = requested
        .into_iter()
        .map(|symbol| symbol.to_ascii_uppercase())
        .filter(|symbol| available.contains(symbol))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if selected.is_empty() && !available.is_empty() {
        bail!("none of the requested symbols exist in the selected range");
    }
    Ok(selected)
}

fn validate_order(order: &IntraAnalysisOrder) -> Result<()> {
    if order.symbol.trim().is_empty() {
        bail!("intra analysis order {} has an empty symbol", order.fkey);
    }
    for (field, value) in [
        ("spot_price", order.spot_price),
        ("futures_price", order.futures_price),
        ("quantity", order.quantity),
    ] {
        if !value.is_finite() || value <= 0.0 {
            bail!(
                "intra analysis order {} has invalid {field}: {value}",
                order.fkey
            );
        }
    }
    if let Some(premium) = order.premium {
        for (field, value) in [
            ("premium.open_rate", premium.open_rate),
            ("premium.high_rate", premium.high_rate),
            ("premium.low_rate", premium.low_rate),
            ("premium.close_rate", premium.close_rate),
        ] {
            if !value.is_finite() {
                bail!(
                    "intra analysis order {} has invalid {field}: {value}",
                    order.fkey
                );
            }
        }
        if premium.high_rate < premium.open_rate
            || premium.high_rate < premium.close_rate
            || premium.low_rate > premium.open_rate
            || premium.low_rate > premium.close_rate
        {
            bail!(
                "intra analysis order {} has invalid premium OHLC",
                order.fkey
            );
        }
    }
    Ok(())
}

fn validate_fee_event(event: &IntraAnalysisFeeEvent) -> Result<()> {
    if event.symbol.trim().is_empty() {
        bail!("intra analysis fee event has an empty symbol");
    }
    if event.ts <= 0 {
        bail!(
            "intra analysis fee event has invalid timestamp: {}",
            event.ts
        );
    }
    if !event.notional_usdt.is_finite() || event.notional_usdt <= 0.0 {
        bail!(
            "intra analysis fee event has invalid notional: {}",
            event.notional_usdt
        );
    }
    if event.fee_usdt.is_some_and(|fee| !fee.is_finite()) {
        bail!("intra analysis fee event has a non-finite fee");
    }
    Ok(())
}

fn validate_funding_event(event: &IntraAnalysisFundingEvent) -> Result<()> {
    if event.symbol.trim().is_empty() {
        bail!("intra analysis funding event has an empty symbol");
    }
    if event.ts <= 0 {
        bail!(
            "intra analysis funding event has invalid timestamp: {}",
            event.ts
        );
    }
    if !event.amount_usdt.is_finite() {
        bail!("intra analysis funding event has a non-finite amount");
    }
    Ok(())
}

fn allocate_funding(state: &mut SymbolState, amount_usdt: f64) -> bool {
    let total_futures_notional = state
        .positive
        .iter()
        .chain(&state.reverse)
        .map(|lot| lot.remaining_quantity * lot.futures_price)
        .sum::<f64>();
    if total_futures_notional <= f64::EPSILON {
        return false;
    }
    for lot in state.positive.iter_mut().chain(&mut state.reverse) {
        let notional = lot.remaining_quantity * lot.futures_price;
        lot.funding_pnl_usdt += amount_usdt * notional / total_futures_notional;
    }
    true
}

fn ensure_final_point(points: &mut Vec<IntraAnalysisPoint>, end_ms: i64) {
    if points.last().is_some_and(|point| point.ts == end_ms) {
        return;
    }
    let mut point = points.last().copied().unwrap_or_default();
    point.ts = end_ms;
    push_or_replace_point(points, point);
}

fn push_or_replace_point(points: &mut Vec<IntraAnalysisPoint>, point: IntraAnalysisPoint) {
    if let Some(last) = points.last_mut()
        && last.ts == point.ts
    {
        *last = point;
    } else {
        points.push(point);
    }
}

fn downsample_points(
    points: Vec<IntraAnalysisPoint>,
    max_points: usize,
) -> Vec<IntraAnalysisPoint> {
    if points.len() <= max_points {
        return points;
    }
    let last = points.len() - 1;
    let slots = max_points - 1;
    (0..max_points)
        .map(|slot| {
            let index = if slot == slots {
                last
            } else {
                slot * last / slots
            };
            points[index]
        })
        .collect()
}

fn ratio(numerator: f64, denominator: f64) -> f64 {
    if denominator.abs() > f64::EPSILON {
        numerator / denominator
    } else {
        0.0
    }
}

fn clean_quantity(value: f64) -> f64 {
    if value.abs() <= QUANTITY_EPSILON {
        0.0
    } else {
        value
    }
}

fn clean_zero(value: f64) -> f64 {
    if value.abs() <= 1e-9 { 0.0 } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(
        fkey: i64,
        direction: ArbDirection,
        completed_at_ms: i64,
        spot_price: f64,
        futures_price: f64,
        quantity: f64,
    ) -> IntraAnalysisOrder {
        IntraAnalysisOrder {
            fkey,
            symbol: "BTCUSDT".to_string(),
            direction,
            completed_at_ms,
            spot_price,
            futures_price,
            quantity,
            premium: None,
        }
    }

    fn request(start_ms: i64, end_ms: i64) -> IntraAnalysisRequest {
        IntraAnalysisRequest {
            strategy_slug: "binance-intra-arb01".to_string(),
            display_name: "binance mt".to_string(),
            premium_adapter: "binance_premium_index_klines_1m",
            strategy_start_ms: 1_000,
            start_ms,
            end_ms,
            selected_symbols: Vec::new(),
            reference_fee_bps: 1.0,
            max_points: 1_000,
            max_matches: 200,
        }
    }

    fn fee_event(
        symbol: &str,
        ts: i64,
        notional_usdt: f64,
        fee_usdt: Option<f64>,
    ) -> IntraAnalysisFeeEvent {
        IntraAnalysisFeeEvent {
            symbol: symbol.to_string(),
            ts,
            notional_usdt,
            fee_usdt,
        }
    }

    fn funding_event(symbol: &str, ts: i64, amount_usdt: f64) -> IntraAnalysisFundingEvent {
        IntraAnalysisFundingEvent {
            symbol: symbol.to_string(),
            ts,
            amount_usdt,
        }
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "actual={actual}, expected={expected}"
        );
    }

    fn with_premium(mut order: IntraAnalysisOrder, close_rate: f64) -> IntraAnalysisOrder {
        order.premium = Some(PremiumIndexCandle {
            open_rate: close_rate,
            high_rate: close_rate,
            low_rate: close_rate,
            close_rate,
        });
        order
    }

    #[test]
    fn positive_then_reverse_realizes_basis_convergence() {
        let response = calculate(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 2.0),
                order(2, ArbDirection::Reverse, 1_200, 105.0, 108.0, 2.0),
            ],
            request(1_000, 1_300),
        )
        .unwrap();

        assert_close(response.summary.realized_pnl_usdt, 14.0);
        assert_close(response.summary.matched_quantity, 2.0);
        assert_close(response.summary.matched_notional_usdt, 205.0);
        assert_eq!(response.summary.closed_match_count, 1);
        assert_eq!(response.summary.winning_match_count, 1);
        assert_eq!(response.summary.positive_open_lot_count, 0);
        assert_eq!(response.summary.reverse_open_lot_count, 0);
        assert_eq!(response.matches[0].open_direction, ArbDirection::Positive);
        assert_close(response.matches[0].entry_basis_bps, 1_000.0);
        assert_close(response.matches[0].exit_basis_bps, 3.0 / 105.0 * 10_000.0);
    }

    #[test]
    fn applies_actual_and_reference_fees_to_closed_four_leg_notional() {
        let response = calculate_with_fees(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 2.0),
                order(2, ArbDirection::Reverse, 1_300, 105.0, 108.0, 2.0),
            ],
            vec![
                fee_event("btcusdt", 1_150, 423.0, Some(0.20)),
                fee_event("BTCUSDT", 1_250, 423.0, Some(-0.05)),
            ],
            request(1_000, 1_400),
        )
        .unwrap();

        assert_close(response.summary.realized_pnl_usdt, 14.0);
        assert_close(response.summary.trading_fee_usdt, 0.15);
        assert_close(response.summary.fee_after_pnl_usdt, 13.85);
        assert_close(response.summary.reference_trading_fee_usdt, 0.0846);
        assert_close(response.summary.reference_fee_after_pnl_usdt, 13.9154);
        assert_eq!(response.summary.fee_trade_count, 2);
        assert_eq!(response.summary.converted_fee_trade_count, 2);
        assert_close(response.summary.actual_fee_coverage, 1.0);
        assert_eq!(response.source.window_fee_trade_rows, 2);
        assert_eq!(response.source.converted_fee_trade_rows, 2);
        let final_point = response.points.last().unwrap();
        assert_close(final_point.fee_after_pnl_usdt, 13.85);
        assert_close(final_point.reference_fee_after_pnl_usdt, 13.9154);
        assert_eq!(response.matches[0].fee_notional_usdt, 846.0);
        assert_close(response.matches[0].trading_fee_usdt, 0.15);
    }

    #[test]
    fn fee_series_preserves_negative_cumulative_rebates() {
        let response = calculate_with_fees(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 1.0),
                order(2, ArbDirection::Reverse, 1_300, 100.0, 105.0, 1.0),
            ],
            vec![fee_event("BTCUSDT", 1_200, 415.0, Some(-0.25))],
            request(1_000, 1_400),
        )
        .unwrap();

        assert_close(response.summary.trading_fee_usdt, -0.25);
        assert_close(response.summary.fee_after_pnl_usdt, 5.25);
        assert_close(response.points.last().unwrap().trading_fee_usdt, -0.25);
        assert_close(response.points.last().unwrap().fee_after_pnl_usdt, 5.25);
    }

    #[test]
    fn releases_funding_only_for_the_fifo_closed_quantity() {
        let response = calculate_with_fees_and_funding(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 1.0),
                order(2, ArbDirection::Positive, 1_150, 200.0, 220.0, 1.0),
                order(3, ArbDirection::Reverse, 1_300, 100.0, 105.0, 1.5),
            ],
            vec![fee_event("BTCUSDT", 1_250, 727.5, Some(3.0))],
            vec![funding_event("btcusdt", 1_200, 30.0)],
            request(1_000, 1_400),
        )
        .unwrap();

        assert_close(response.summary.realized_pnl_usdt, 12.5);
        assert_close(response.summary.funding_pnl_usdt, 20.0);
        assert_close(response.summary.gross_pnl_usdt, 32.5);
        assert_close(response.summary.trading_fee_usdt, 3.0);
        assert_close(response.summary.fee_after_pnl_usdt, 29.5);
        assert_close(response.summary.reference_trading_fee_usdt, 0.07275);
        assert_close(response.summary.reference_fee_after_pnl_usdt, 32.42725);
        assert_close(response.matches[0].funding_pnl_usdt, 10.0);
        assert_close(response.matches[1].funding_pnl_usdt, 10.0);
        assert_close(response.summary.positive_open_quantity, 0.5);
        assert_close(response.points.last().unwrap().funding_pnl_usdt, 20.0);
        assert_close(response.points.last().unwrap().gross_pnl_usdt, 32.5);
        assert_close(response.points.last().unwrap().fee_after_pnl_usdt, 29.5);
        assert_eq!(response.source.window_funding_rows, 1);
        assert_eq!(response.source.allocated_funding_rows, 1);
        assert!(response.source.funding_included);
    }

    #[test]
    fn assigns_window_funding_to_a_pre_window_fifo_seed() {
        let response = calculate_with_fees_and_funding(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 1.0),
                order(2, ArbDirection::Reverse, 2_100, 100.0, 105.0, 1.0),
            ],
            Vec::new(),
            vec![
                funding_event("BTCUSDT", 1_900, 9.0),
                funding_event("BTCUSDT", 2_050, 3.0),
            ],
            request(2_000, 2_200),
        )
        .unwrap();

        assert_close(response.summary.realized_pnl_usdt, 5.0);
        assert_close(response.summary.funding_pnl_usdt, 3.0);
        assert_close(response.summary.gross_pnl_usdt, 8.0);
        assert_eq!(response.source.loaded_funding_rows, 2);
        assert_eq!(response.source.window_funding_rows, 1);
        assert_eq!(response.source.allocated_funding_rows, 1);
    }

    #[test]
    fn ignores_funding_without_an_open_fifo_lot() {
        let response = calculate_with_fees_and_funding(
            vec![
                order(1, ArbDirection::Positive, 1_200, 100.0, 110.0, 1.0),
                order(2, ArbDirection::Reverse, 1_300, 100.0, 105.0, 1.0),
            ],
            Vec::new(),
            vec![funding_event("BTCUSDT", 1_100, 7.0)],
            request(1_000, 1_400),
        )
        .unwrap();

        assert_close(response.summary.funding_pnl_usdt, 0.0);
        assert_close(response.summary.gross_pnl_usdt, 5.0);
        assert_eq!(response.source.window_funding_rows, 1);
        assert_eq!(response.source.allocated_funding_rows, 0);
    }

    #[test]
    fn reverse_then_positive_uses_the_opposite_basis_sign() {
        let response = calculate(
            vec![
                order(1, ArbDirection::Reverse, 1_100, 110.0, 100.0, 3.0),
                order(2, ArbDirection::Positive, 1_200, 105.0, 101.0, 3.0),
            ],
            request(1_000, 1_300),
        )
        .unwrap();

        assert_close(response.summary.realized_pnl_usdt, 18.0);
        assert_eq!(response.matches[0].open_direction, ArbDirection::Reverse);
    }

    #[test]
    fn decomposes_realized_pnl_into_market_and_execution_components() {
        let response = calculate(
            vec![
                with_premium(
                    order(1, ArbDirection::Reverse, 1_100, 100.0, 90.0, 2.0),
                    -0.08,
                ),
                with_premium(
                    order(2, ArbDirection::Positive, 1_200, 100.0, 95.0, 2.0),
                    -0.04,
                ),
            ],
            request(1_000, 1_300),
        )
        .unwrap();

        assert_close(response.summary.realized_pnl_usdt, 10.0);
        assert_close(response.summary.market_pnl_usdt, 8.0);
        assert_close(response.summary.execution_pnl_usdt, 2.0);
        assert_close(response.summary.execution_capture_usdt, 2.0);
        assert_close(response.summary.positive_execution_capture_usdt, -2.0);
        assert_close(response.summary.reverse_execution_capture_usdt, 4.0);
        assert_close(response.summary.premium_coverage, 1.0);
        assert_close(response.summary.execution_mt_premium_coverage, 1.0);
        assert_eq!(response.summary.decomposed_match_count, 1);
        assert_eq!(response.summary.execution_mt_count, 2);
        assert_close(response.matches[0].entry_premium_bps.unwrap(), -800.0);
        assert_close(response.matches[0].exit_premium_bps.unwrap(), -400.0);
        assert_close(
            response.matches[0].entry_execution_edge_bps.unwrap(),
            -200.0,
        );
        assert_close(response.matches[0].exit_execution_edge_bps.unwrap(), -100.0);
        assert_close(
            response.matches[0].market_pnl_usdt.unwrap()
                + response.matches[0].execution_pnl_usdt.unwrap(),
            response.matches[0].pnl_usdt,
        );
        assert_close(
            response.matches[0].entry_execution_pnl_usdt.unwrap()
                + response.matches[0].exit_execution_pnl_usdt.unwrap(),
            response.matches[0].execution_pnl_usdt.unwrap(),
        );
    }

    #[test]
    fn closes_opposite_combinations_fifo_by_base_quantity() {
        let response = calculate(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 1.0),
                order(2, ArbDirection::Positive, 1_200, 100.0, 108.0, 2.0),
                order(3, ArbDirection::Reverse, 1_300, 100.0, 105.0, 1.5),
            ],
            request(1_000, 1_400),
        )
        .unwrap();

        assert_close(response.summary.realized_pnl_usdt, 6.5);
        assert_eq!(response.summary.closed_match_count, 2);
        assert_close(response.summary.positive_open_quantity, 1.5);
        assert_close(response.summary.positive_open_notional_usdt, 150.0);
        assert_close(response.summary.positive_average_basis_bps, 800.0);
        assert_eq!(response.matches[0].open_fkey, 2);
        assert_eq!(response.matches[1].open_fkey, 1);
    }

    #[test]
    fn pre_window_orders_seed_fifo_without_entering_window_counts() {
        let response = calculate(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 1.0),
                order(2, ArbDirection::Reverse, 2_100, 100.0, 105.0, 1.0),
            ],
            request(2_000, 2_200),
        )
        .unwrap();

        assert_eq!(response.summary.mt_count, 1);
        assert_eq!(response.summary.reverse_mt_count, 1);
        assert_eq!(response.summary.positive_mt_count, 0);
        assert_close(response.summary.realized_pnl_usdt, 5.0);
        assert_eq!(response.points.first().unwrap().realized_pnl_usdt, 0.0);
        assert_close(response.points.last().unwrap().realized_pnl_usdt, 5.0);
    }

    #[test]
    fn window_execution_diagnostic_stays_out_of_closed_pnl_series() {
        let response = calculate(
            vec![
                with_premium(
                    order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 1.0),
                    0.08,
                ),
                with_premium(
                    order(2, ArbDirection::Reverse, 2_100, 100.0, 105.0, 1.0),
                    0.06,
                ),
            ],
            request(2_000, 2_200),
        )
        .unwrap();

        assert_close(response.summary.realized_pnl_usdt, 5.0);
        assert_close(response.summary.market_pnl_usdt, 2.0);
        assert_close(response.summary.execution_pnl_usdt, 3.0);
        assert_eq!(response.summary.execution_mt_count, 1);
        assert_close(response.summary.execution_capture_usdt, 1.0);
        assert_close(response.summary.positive_execution_capture_usdt, 0.0);
        assert_close(response.summary.reverse_execution_capture_usdt, 1.0);
        assert_close(response.matches[0].entry_execution_pnl_usdt.unwrap(), 2.0);
        assert_close(response.matches[0].exit_execution_pnl_usdt.unwrap(), 1.0);
        assert_close(response.points.last().unwrap().execution_capture_usdt, 0.0);
    }

    #[test]
    fn keeps_unselected_symbols_out_of_portfolio_summary() {
        let mut eth_open = order(3, ArbDirection::Positive, 1_100, 100.0, 120.0, 1.0);
        eth_open.symbol = "ETHUSDT".to_string();
        let mut eth_close = order(4, ArbDirection::Reverse, 1_200, 100.0, 100.0, 1.0);
        eth_close.symbol = "ETHUSDT".to_string();
        let mut calculation = request(1_000, 1_300);
        calculation.selected_symbols = vec!["BTCUSDT".to_string()];

        let response = calculate(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 1.0),
                order(2, ArbDirection::Reverse, 1_200, 100.0, 105.0, 1.0),
                eth_open,
                eth_close,
            ],
            calculation,
        )
        .unwrap();

        assert_eq!(response.available_symbols, vec!["BTCUSDT", "ETHUSDT"]);
        assert_eq!(response.selected_symbols, vec!["BTCUSDT"]);
        assert_close(response.summary.realized_pnl_usdt, 5.0);
        assert_eq!(response.symbol_points.len(), 1);
        assert_eq!(response.symbol_points[0].symbol, "BTCUSDT");
        assert_close(
            response.symbol_points[0]
                .points
                .last()
                .unwrap()
                .realized_pnl_usdt,
            5.0,
        );
    }

    #[test]
    fn unselected_symbol_retains_its_fee_breakdown() {
        let mut eth_open = order(3, ArbDirection::Positive, 1_100, 100.0, 120.0, 1.0);
        eth_open.symbol = "ETHUSDT".to_string();
        let mut eth_close = order(4, ArbDirection::Reverse, 1_200, 100.0, 100.0, 1.0);
        eth_close.symbol = "ETHUSDT".to_string();
        let mut calculation = request(1_000, 1_300);
        calculation.selected_symbols = vec!["BTCUSDT".to_string()];

        let response = calculate_with_fees(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 1.0),
                order(2, ArbDirection::Reverse, 1_200, 100.0, 105.0, 1.0),
                eth_open,
                eth_close,
            ],
            vec![
                fee_event("BTCUSDT", 1_150, 415.0, Some(0.1)),
                fee_event("ETHUSDT", 1_150, 420.0, Some(0.3)),
            ],
            calculation,
        )
        .unwrap();

        assert_close(response.summary.trading_fee_usdt, 0.1);
        assert_eq!(response.source.window_fee_trade_rows, 1);
        let eth = response
            .symbols
            .iter()
            .find(|row| row.symbol == "ETHUSDT")
            .unwrap();
        assert_close(eth.summary.trading_fee_usdt, 0.3);
        assert_close(eth.summary.reference_trading_fee_usdt, 0.042);
    }

    #[test]
    fn excludes_unmatched_quantity_from_gross_and_fee_results() {
        let response = calculate_with_fees(
            vec![
                order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 2.0),
                order(2, ArbDirection::Reverse, 1_300, 105.0, 108.0, 1.0),
            ],
            vec![fee_event("BTCUSDT", 1_200, 625.0, Some(0.625))],
            request(1_000, 1_400),
        )
        .unwrap();

        assert_close(response.summary.matched_quantity, 1.0);
        assert_close(response.summary.realized_pnl_usdt, 7.0);
        assert_close(response.summary.trading_fee_usdt, 0.423);
        assert_close(response.summary.reference_trading_fee_usdt, 0.0423);
        assert_close(response.summary.positive_open_quantity, 1.0);
        assert_eq!(response.summary.closed_match_count, 1);
        assert_close(response.matches[0].fee_notional_usdt, 423.0);
    }

    #[test]
    fn symbol_series_sum_to_the_portfolio_series() {
        let mut eth_open = with_premium(
            order(3, ArbDirection::Reverse, 1_150, 100.0, 92.0, 2.0),
            -0.06,
        );
        eth_open.symbol = "ETHUSDT".to_string();
        let mut eth_close = with_premium(
            order(4, ArbDirection::Positive, 1_250, 100.0, 96.0, 2.0),
            -0.03,
        );
        eth_close.symbol = "ETHUSDT".to_string();

        let response = calculate(
            vec![
                with_premium(
                    order(1, ArbDirection::Positive, 1_100, 100.0, 110.0, 1.0),
                    0.08,
                ),
                with_premium(
                    order(2, ArbDirection::Reverse, 1_200, 100.0, 105.0, 1.0),
                    0.06,
                ),
                eth_open,
                eth_close,
            ],
            request(1_000, 1_300),
        )
        .unwrap();

        assert_eq!(response.symbol_points.len(), 2);
        let final_points = response
            .symbol_points
            .iter()
            .map(|series| *series.points.last().unwrap())
            .collect::<Vec<_>>();
        let portfolio = response.points.last().unwrap();
        assert_close(
            final_points
                .iter()
                .map(|point| point.realized_pnl_usdt)
                .sum(),
            portfolio.realized_pnl_usdt,
        );
        assert_close(
            final_points.iter().map(|point| point.market_pnl_usdt).sum(),
            portfolio.market_pnl_usdt,
        );
        assert_close(
            final_points
                .iter()
                .map(|point| point.execution_pnl_usdt)
                .sum(),
            portfolio.execution_pnl_usdt,
        );
        assert_close(
            final_points
                .iter()
                .map(|point| point.execution_capture_usdt)
                .sum(),
            portfolio.execution_capture_usdt,
        );
        assert_eq!(
            response.source.returned_symbol_points,
            response
                .symbol_points
                .iter()
                .map(|series| series.points.len())
                .sum::<usize>()
        );
    }
}
