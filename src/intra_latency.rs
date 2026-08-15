//! Hourly intra-arb order latency from persist uniform_orders rows.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env,
    path::PathBuf,
    process::Command,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tracing::{error, info};

const DEFAULT_SYNC_INTERVAL_SECS: u64 = 3_600;
const SYNC_INTERVAL_ENV: &str = "CRYPTO_NAV_HOURLY_LATENCY_SECS";

pub const HOUR_MS: i64 = 3_600_000;
pub const NORMAL_MAX_MS: f64 = 100.0;
pub const SUPPORTED_STRATEGY_SLUGS: [&str; 2] = ["binance-intra-arb01", "bybit-intra-arb01"];

#[derive(Clone, Debug, PartialEq)]
pub struct LatencyOrderRow {
    pub ts_us: i64,
    pub trading_venue: String,
    pub status: String,
    pub create_ts: i64,
    pub update_ts: i64,
    pub signal_ts: i64,
    pub signal_open_ts: i64,
    pub signal_hedge_ts: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyQuantiles {
    pub sample_count: u64,
    pub normal_count: u64,
    pub p50_ms: Option<f64>,
    pub p90_ms: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HourlyLatencyPoint {
    pub strategy_slug: String,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub computed_at_ms: i64,
    pub margin_new_create: LatencyQuantiles,
    pub futures_new_create: LatencyQuantiles,
    pub spot_trigger: LatencyQuantiles,
    pub futures_trigger: LatencyQuantiles,
}

impl HourlyLatencyPoint {
    pub fn from_stored_parts(
        strategy_slug: String,
        window_start_ms: i64,
        window_end_ms: i64,
        computed_at_ms: i64,
        margin_new_create: LatencyQuantiles,
        futures_new_create: LatencyQuantiles,
        spot_trigger: LatencyQuantiles,
        futures_trigger: LatencyQuantiles,
    ) -> Self {
        Self {
            strategy_slug,
            window_start_ms,
            window_end_ms,
            computed_at_ms,
            margin_new_create,
            futures_new_create,
            spot_trigger,
            futures_trigger,
        }
    }
}

impl LatencyQuantiles {
    pub fn from_stored(
        sample_count: u64,
        normal_count: u64,
        p50_ms: Option<f64>,
        p90_ms: Option<f64>,
    ) -> Self {
        Self {
            sample_count,
            normal_count,
            p50_ms,
            p90_ms,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HourlyLatencySeries {
    pub strategy_slug: String,
    pub points: Vec<HourlyLatencyPoint>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TriggerLeg {
    Spot,
    Futures,
}

pub fn supports_hourly_latency(slug: &str) -> bool {
    SUPPORTED_STRATEGY_SLUGS.contains(&slug)
}

pub fn floor_hour_ms(ts_ms: i64) -> Result<i64> {
    if ts_ms < 0 {
        bail!("timestamp must be non-negative: {ts_ms}");
    }
    Ok(ts_ms - ts_ms.rem_euclid(HOUR_MS))
}

pub fn last_complete_hour_end_ms(now_ms: i64) -> Result<i64> {
    floor_hour_ms(now_ms)
}

pub fn last_complete_hour_start_ms(now_ms: i64) -> Result<i64> {
    Ok(last_complete_hour_end_ms(now_ms)?.saturating_sub(HOUR_MS))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HourWindow {
    pub start_ms: i64,
    pub end_ms: i64,
}

impl HourWindow {
    pub fn new(start_ms: i64) -> Result<Self> {
        if start_ms.rem_euclid(HOUR_MS) != 0 {
            bail!("latency window must start on an hour boundary");
        }
        let end_ms = start_ms
            .checked_add(HOUR_MS)
            .context("hour window overflow")?;
        Ok(Self { start_ms, end_ms })
    }
}

/// Expand an optional hour-aligned range into complete UTC hour buckets.
///
/// An omitted range is the last complete UTC hour at `now_ms`. An explicit
/// range yields every complete hour `[start, start+1h)` whose start is
/// `>= range_start` and whose end is `<= min(range_end, last_complete_hour_end)`.
pub fn planned_hour_windows(
    now_ms: i64,
    range_start_ms: Option<i64>,
    range_end_ms: Option<i64>,
) -> Result<Vec<HourWindow>> {
    let last_complete_end_ms = last_complete_hour_end_ms(now_ms)?;
    if last_complete_end_ms == 0 {
        bail!("no complete hour is available yet");
    }
    let default_start_ms = last_complete_hour_start_ms(now_ms)?;
    let start_ms = range_start_ms.unwrap_or(default_start_ms);
    let requested_end_ms = range_end_ms.unwrap_or(start_ms.saturating_add(HOUR_MS));
    if start_ms.rem_euclid(HOUR_MS) != 0 {
        bail!("--window-start-ms must be aligned to an hour");
    }
    if requested_end_ms.rem_euclid(HOUR_MS) != 0 {
        bail!("--window-end-ms must be aligned to an hour");
    }
    if requested_end_ms <= start_ms {
        bail!("recompute range end must be after start");
    }
    let end_ms = requested_end_ms.min(last_complete_end_ms);
    if end_ms <= start_ms {
        bail!("no complete hour is available in the requested range");
    }
    let mut windows = Vec::new();
    let mut cursor = start_ms;
    while cursor < end_ms {
        windows.push(HourWindow::new(cursor)?);
        cursor = cursor
            .checked_add(HOUR_MS)
            .context("hour window overflow")?;
    }
    if windows.is_empty() {
        bail!("no complete hour is available in the requested range");
    }
    Ok(windows)
}

pub fn classify_trigger_leg(open_ts: i64, hedge_ts: i64) -> Option<TriggerLeg> {
    if open_ts <= 0 || hedge_ts <= 0 {
        return None;
    }
    if open_ts > hedge_ts {
        Some(TriggerLeg::Spot)
    } else if hedge_ts > open_ts {
        Some(TriggerLeg::Futures)
    } else {
        None
    }
}

fn is_new_status(status: &str) -> bool {
    status.eq_ignore_ascii_case("NEW")
}

fn is_margin_venue(venue: &str) -> bool {
    venue.eq_ignore_ascii_case("BinanceMargin") || venue.eq_ignore_ascii_case("BybitMargin")
}

fn is_futures_venue(venue: &str) -> bool {
    venue.eq_ignore_ascii_case("BinanceFutures") || venue.eq_ignore_ascii_case("BybitFutures")
}

fn latency_ms(lhs: i64, rhs: i64) -> Option<f64> {
    if lhs <= 0 || rhs <= 0 {
        return None;
    }
    Some((lhs - rhs) as f64 / 1_000.0)
}

pub fn quantiles(samples: &[f64], normal_max_ms: f64) -> LatencyQuantiles {
    let mut normal = samples
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value >= 0.0 && *value <= normal_max_ms)
        .collect::<Vec<_>>();
    normal.sort_by(|left, right| left.total_cmp(right));
    LatencyQuantiles {
        sample_count: samples.len() as u64,
        normal_count: normal.len() as u64,
        p50_ms: percentile(&normal, 0.50),
        p90_ms: percentile(&normal, 0.90),
    }
}

fn percentile(sorted: &[f64], q: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let max_index = sorted.len() - 1;
    let rank = q * max_index as f64;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    if lower == upper {
        return Some(sorted[lower]);
    }
    let weight = rank - lower as f64;
    Some(sorted[lower] * (1.0 - weight) + sorted[upper] * weight)
}

pub fn compute_hourly_latency(
    strategy_slug: &str,
    window_start_ms: i64,
    window_end_ms: i64,
    computed_at_ms: i64,
    rows: &[LatencyOrderRow],
) -> Result<HourlyLatencyPoint> {
    if !supports_hourly_latency(strategy_slug) {
        bail!("hourly latency is not configured for {strategy_slug}");
    }
    if window_end_ms <= window_start_ms {
        bail!("latency window end must be after start");
    }
    if window_end_ms - window_start_ms != HOUR_MS {
        bail!("latency window must be exactly one hour");
    }
    if window_start_ms.rem_euclid(HOUR_MS) != 0 {
        bail!("latency window must start on an hour boundary");
    }

    let window_start_us = window_start_ms.saturating_mul(1_000);
    let window_end_us = window_end_ms.saturating_mul(1_000);
    let mut margin_new_create = Vec::new();
    let mut futures_new_create = Vec::new();
    let mut spot_trigger = Vec::new();
    let mut futures_trigger = Vec::new();

    for row in rows {
        if row.ts_us < window_start_us || row.ts_us >= window_end_us {
            continue;
        }
        if !is_new_status(&row.status) {
            continue;
        }
        if is_margin_venue(&row.trading_venue) {
            if let Some(value) = latency_ms(row.update_ts, row.create_ts) {
                margin_new_create.push(value);
            }
            if row.signal_ts > 0 {
                match classify_trigger_leg(row.signal_open_ts, row.signal_hedge_ts) {
                    Some(TriggerLeg::Spot) => {
                        if let Some(value) = latency_ms(row.signal_ts, row.signal_open_ts) {
                            spot_trigger.push(value);
                        }
                    }
                    Some(TriggerLeg::Futures) => {
                        if let Some(value) = latency_ms(row.signal_ts, row.signal_hedge_ts) {
                            futures_trigger.push(value);
                        }
                    }
                    None => {}
                }
            }
        } else if is_futures_venue(&row.trading_venue) {
            if let Some(value) = latency_ms(row.update_ts, row.create_ts) {
                futures_new_create.push(value);
            }
        }
    }

    Ok(HourlyLatencyPoint {
        strategy_slug: strategy_slug.to_string(),
        window_start_ms,
        window_end_ms,
        computed_at_ms,
        margin_new_create: quantiles(&margin_new_create, NORMAL_MAX_MS),
        futures_new_create: quantiles(&futures_new_create, NORMAL_MAX_MS),
        spot_trigger: quantiles(&spot_trigger, NORMAL_MAX_MS),
        futures_trigger: quantiles(&futures_trigger, NORMAL_MAX_MS),
    })
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HourlyLatencyStore {
    rows: BTreeMap<(String, i64), HourlyLatencyPoint>,
}

impl HourlyLatencyStore {
    pub fn upsert(&mut self, point: HourlyLatencyPoint) {
        self.rows
            .insert((point.strategy_slug.clone(), point.window_start_ms), point);
    }

    pub fn load(&self, strategy_slug: &str, start_ms: i64, end_ms: i64) -> HourlyLatencySeries {
        let points = self
            .rows
            .values()
            .filter(|point| {
                point.strategy_slug == strategy_slug
                    && point.window_start_ms >= start_ms
                    && point.window_start_ms < end_ms
            })
            .cloned()
            .collect();
        HourlyLatencySeries {
            strategy_slug: strategy_slug.to_string(),
            points,
        }
    }
}

pub fn spawn() -> Result<()> {
    let Some(config) = HourlyLatencyConfig::from_env()? else {
        info!("hourly intra latency sync disabled");
        return Ok(());
    };
    info!(
        sync_interval_secs = config.sync_interval.as_secs(),
        binary = %config.sync_bin.display(),
        "hourly intra latency sync enabled"
    );
    tokio::spawn(run_hourly_latency_loop(config));
    Ok(())
}

struct HourlyLatencyConfig {
    sync_interval: Duration,
    sync_bin: PathBuf,
}

impl HourlyLatencyConfig {
    fn from_env() -> Result<Option<Self>> {
        let sync_interval_secs = match env::var(SYNC_INTERVAL_ENV) {
            Ok(value) => value
                .parse::<u64>()
                .with_context(|| format!("{SYNC_INTERVAL_ENV} must be an integer"))?,
            Err(env::VarError::NotPresent) => DEFAULT_SYNC_INTERVAL_SECS,
            Err(env::VarError::NotUnicode(_)) => {
                bail!("{SYNC_INTERVAL_ENV} is not valid Unicode")
            }
        };
        if sync_interval_secs == 0 {
            return Ok(None);
        }
        if sync_interval_secs % 60 != 0 {
            bail!("{SYNC_INTERVAL_ENV} must be a whole number of minutes");
        }
        let sync_bin = env::var_os("CRYPTO_NAV_HOURLY_LATENCY_BIN")
            .map(PathBuf::from)
            .unwrap_or(
                env::current_exe()
                    .context("resolve NAV server executable")?
                    .with_file_name("sync_intra_hourly_latency"),
            );
        Ok(Some(Self {
            sync_interval: Duration::from_secs(sync_interval_secs),
            sync_bin,
        }))
    }
}

async fn run_hourly_latency_loop(config: HourlyLatencyConfig) {
    loop {
        let (delay, next_sync_at_ms) = match delay_until_next_hour(SystemTime::now()) {
            Ok(schedule) => schedule,
            Err(error) => {
                error!(error = ?error, "calculate hourly latency schedule failed");
                tokio::time::sleep(Duration::from_secs(30)).await;
                continue;
            }
        };
        info!(next_sync_at_ms, "hourly intra latency sync scheduled");
        tokio::time::sleep(delay).await;
        let task_config = config.sync_bin.clone();
        match tokio::task::spawn_blocking(move || run_hourly_latency_bin(&task_config)).await {
            Ok(Ok(summary)) => info!(summary = %summary, "hourly intra latency sync complete"),
            Ok(Err(error)) => error!(error = ?error, "hourly intra latency sync failed"),
            Err(error) => error!(error = ?error, "join hourly intra latency sync failed"),
        }
    }
}

fn delay_until_next_hour(now: SystemTime) -> Result<(Duration, i64)> {
    let now_ms = i64::try_from(
        now.duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_millis(),
    )
    .context("system clock milliseconds overflow i64")?;
    let next_ms = floor_hour_ms(now_ms)?
        .checked_add(HOUR_MS)
        .context("next hour overflow")?;
    Ok((Duration::from_millis((next_ms - now_ms) as u64), next_ms))
}

fn run_hourly_latency_bin(bin: &PathBuf) -> Result<String> {
    let output = Command::new(bin)
        .output()
        .with_context(|| format!("run {}", bin.display()))?;
    if !output.status.success() {
        bail!(
            "{} exited with {}: {}",
            bin.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("completed")
        .trim()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        ts_us: i64,
        venue: &str,
        create_ts: i64,
        update_ts: i64,
        signal_ts: i64,
        open_ts: i64,
        hedge_ts: i64,
    ) -> LatencyOrderRow {
        LatencyOrderRow {
            ts_us,
            trading_venue: venue.to_string(),
            status: "NEW".to_string(),
            create_ts,
            update_ts,
            signal_ts,
            signal_open_ts: open_ts,
            signal_hedge_ts: hedge_ts,
        }
    }

    #[test]
    fn classifies_later_bbo_leg_and_ignores_ties() {
        assert_eq!(classify_trigger_leg(20, 10), Some(TriggerLeg::Spot));
        assert_eq!(classify_trigger_leg(10, 20), Some(TriggerLeg::Futures));
        assert_eq!(classify_trigger_leg(10, 10), None);
        assert_eq!(classify_trigger_leg(0, 20), None);
    }

    #[test]
    fn interpolates_quantiles_on_normal_path_only() {
        let stats = quantiles(&[10.0, 20.0, 150.0, -1.0], NORMAL_MAX_MS);
        assert_eq!(stats.sample_count, 4);
        assert_eq!(stats.normal_count, 2);
        assert_eq!(stats.p50_ms, Some(15.0));
        assert_eq!(stats.p90_ms, Some(19.0));
    }

    #[test]
    fn computes_margin_futures_and_trigger_split_for_last_hour() {
        let start_ms = 1_704_067_200_000; // 2023-12-31T16:00:00Z
        let start_us = start_ms * 1_000;
        let rows = vec![
            row(
                start_us + 1_000,
                "BinanceMargin",
                start_us,
                start_us + 1_200,
                start_us + 400,
                start_us + 100,
                start_us + 50,
            ),
            row(
                start_us + 2_000,
                "BinanceMargin",
                start_us,
                start_us + 2_000,
                start_us + 900,
                start_us + 100,
                start_us + 500,
            ),
            row(
                start_us + 3_000,
                "BinanceFutures",
                start_us,
                start_us + 3_000,
                0,
                0,
                0,
            ),
            row(
                start_us + 4_000,
                "BybitMargin",
                start_us,
                start_us + 4_000,
                start_us + 700,
                start_us + 200,
                start_us + 100,
            ),
            row(
                start_us - 1,
                "BinanceMargin",
                start_us,
                start_us + 9_000,
                start_us + 800,
                start_us + 700,
                start_us + 100,
            ),
        ];

        let point = compute_hourly_latency(
            "binance-intra-arb01",
            start_ms,
            start_ms + HOUR_MS,
            start_ms + HOUR_MS,
            &rows,
        )
        .expect("compute hourly latency");

        assert_eq!(point.margin_new_create.sample_count, 3);
        assert_eq!(point.margin_new_create.p50_ms, Some(2.0));
        assert_eq!(point.futures_new_create.sample_count, 1);
        assert_eq!(point.futures_new_create.p50_ms, Some(3.0));
        assert_eq!(point.spot_trigger.sample_count, 2);
        assert_eq!(point.spot_trigger.p50_ms, Some(0.4));
        assert_eq!(point.futures_trigger.sample_count, 1);
        assert_eq!(point.futures_trigger.p50_ms, Some(0.4));
    }

    #[test]
    fn persists_computed_points_and_reads_them_back_for_both_strategies() {
        let start_ms = HOUR_MS * 100;
        let mut store = HourlyLatencyStore::default();
        for slug in SUPPORTED_STRATEGY_SLUGS {
            let rows = vec![
                row(
                    start_ms * 1_000 + 1_000,
                    if slug.starts_with("binance") {
                        "BinanceMargin"
                    } else {
                        "BybitMargin"
                    },
                    start_ms * 1_000,
                    start_ms * 1_000 + 1_500,
                    start_ms * 1_000 + 800,
                    start_ms * 1_000 + 200,
                    start_ms * 1_000 + 100,
                ),
                row(
                    start_ms * 1_000 + 2_000,
                    if slug.starts_with("binance") {
                        "BinanceFutures"
                    } else {
                        "BybitFutures"
                    },
                    start_ms * 1_000,
                    start_ms * 1_000 + 2_500,
                    0,
                    0,
                    0,
                ),
                row(
                    start_ms * 1_000 + 3_000,
                    if slug.starts_with("binance") {
                        "BinanceMargin"
                    } else {
                        "BybitMargin"
                    },
                    start_ms * 1_000,
                    start_ms * 1_000 + 1_800,
                    start_ms * 1_000 + 900,
                    start_ms * 1_000 + 100,
                    start_ms * 1_000 + 400,
                ),
            ];
            let point = compute_hourly_latency(
                slug,
                start_ms,
                start_ms + HOUR_MS,
                start_ms + HOUR_MS,
                &rows,
            )
            .expect("compute last-hour latency");
            assert!(point.margin_new_create.p50_ms.is_some());
            assert!(point.margin_new_create.p90_ms.is_some());
            assert!(point.futures_new_create.p50_ms.is_some());
            assert!(point.futures_new_create.p90_ms.is_some());
            assert!(point.spot_trigger.p50_ms.is_some());
            assert!(point.spot_trigger.p90_ms.is_some());
            assert!(point.futures_trigger.p50_ms.is_some());
            assert!(point.futures_trigger.p90_ms.is_some());
            store.upsert(point.clone());
            let loaded = store.load(slug, start_ms, start_ms + HOUR_MS);
            assert_eq!(loaded.strategy_slug, slug);
            let restored = loaded
                .points
                .into_iter()
                .map(|stored| {
                    HourlyLatencyPoint::from_stored_parts(
                        stored.strategy_slug,
                        stored.window_start_ms,
                        stored.window_end_ms,
                        stored.computed_at_ms,
                        LatencyQuantiles::from_stored(
                            stored.margin_new_create.sample_count,
                            stored.margin_new_create.normal_count,
                            stored.margin_new_create.p50_ms,
                            stored.margin_new_create.p90_ms,
                        ),
                        LatencyQuantiles::from_stored(
                            stored.futures_new_create.sample_count,
                            stored.futures_new_create.normal_count,
                            stored.futures_new_create.p50_ms,
                            stored.futures_new_create.p90_ms,
                        ),
                        LatencyQuantiles::from_stored(
                            stored.spot_trigger.sample_count,
                            stored.spot_trigger.normal_count,
                            stored.spot_trigger.p50_ms,
                            stored.spot_trigger.p90_ms,
                        ),
                        LatencyQuantiles::from_stored(
                            stored.futures_trigger.sample_count,
                            stored.futures_trigger.normal_count,
                            stored.futures_trigger.p50_ms,
                            stored.futures_trigger.p90_ms,
                        ),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(restored, vec![point]);
        }
    }

    #[test]
    fn rejects_unsupported_strategy() {
        let err =
            compute_hourly_latency("bybit-intra-arb02", HOUR_MS, HOUR_MS * 2, HOUR_MS * 2, &[])
                .unwrap_err();
        assert!(err.to_string().contains("not configured"));
    }

    #[test]
    fn schedules_the_next_utc_hour_boundary() {
        let now = UNIX_EPOCH + Duration::from_millis(HOUR_MS as u64 + 90_000);
        let (delay, next_ms) = delay_until_next_hour(now).expect("schedule next hour");
        assert_eq!(next_ms, HOUR_MS * 2);
        assert_eq!(delay, Duration::from_millis(3_510_000));
    }

    #[test]
    fn omitted_range_plans_exactly_the_last_complete_utc_hour() {
        let now_ms = HOUR_MS * 5 + 90_000;
        let windows = planned_hour_windows(now_ms, None, None).expect("default last hour");
        assert_eq!(
            windows,
            vec![HourWindow {
                start_ms: HOUR_MS * 4,
                end_ms: HOUR_MS * 5,
            }]
        );
    }

    #[test]
    fn hour_aligned_range_plans_one_snapshot_per_complete_hour() {
        let now_ms = HOUR_MS * 10 + 1;
        let windows = planned_hour_windows(now_ms, Some(HOUR_MS * 7), Some(HOUR_MS * 10))
            .expect("multi-hour range");
        assert_eq!(
            windows,
            vec![
                HourWindow {
                    start_ms: HOUR_MS * 7,
                    end_ms: HOUR_MS * 8,
                },
                HourWindow {
                    start_ms: HOUR_MS * 8,
                    end_ms: HOUR_MS * 9,
                },
                HourWindow {
                    start_ms: HOUR_MS * 9,
                    end_ms: HOUR_MS * 10,
                },
            ]
        );
    }

    #[test]
    fn range_end_past_now_stops_at_the_last_complete_hour() {
        let now_ms = HOUR_MS * 3 + 12_000;
        let windows = planned_hour_windows(now_ms, Some(HOUR_MS), Some(HOUR_MS * 9))
            .expect("clip incomplete hours");
        assert_eq!(windows.first().map(|window| window.start_ms), Some(HOUR_MS));
        assert_eq!(
            windows.last().map(|window| window.end_ms),
            Some(HOUR_MS * 3)
        );
        assert_eq!(windows.len(), 2);
    }

    #[test]
    fn rejects_unaligned_recompute_bounds() {
        let now_ms = HOUR_MS * 4;
        let start_err =
            planned_hour_windows(now_ms, Some(HOUR_MS + 1), Some(HOUR_MS * 3)).unwrap_err();
        assert!(start_err.to_string().contains("aligned"));
        let end_err =
            planned_hour_windows(now_ms, Some(HOUR_MS), Some(HOUR_MS * 3 + 1)).unwrap_err();
        assert!(end_err.to_string().contains("aligned"));
    }

    #[test]
    fn recomputes_every_complete_hour_and_reads_the_upserted_snapshots_back() {
        let now_ms = HOUR_MS * 12 + 1;
        let windows = planned_hour_windows(now_ms, Some(HOUR_MS * 10), Some(HOUR_MS * 12))
            .expect("plan two complete hours");
        assert_eq!(windows.len(), 2);
        let mut store = HourlyLatencyStore::default();
        for slug in SUPPORTED_STRATEGY_SLUGS {
            for window in &windows {
                let rows = vec![
                    row(
                        window.start_ms * 1_000 + 1_000,
                        if slug.starts_with("binance") {
                            "BinanceMargin"
                        } else {
                            "BybitMargin"
                        },
                        window.start_ms * 1_000,
                        window.start_ms * 1_000 + 1_500,
                        window.start_ms * 1_000 + 800,
                        window.start_ms * 1_000 + 200,
                        window.start_ms * 1_000 + 100,
                    ),
                    row(
                        window.start_ms * 1_000 + 2_000,
                        if slug.starts_with("binance") {
                            "BinanceFutures"
                        } else {
                            "BybitFutures"
                        },
                        window.start_ms * 1_000,
                        window.start_ms * 1_000 + 2_500,
                        0,
                        0,
                        0,
                    ),
                ];
                let point =
                    compute_hourly_latency(slug, window.start_ms, window.end_ms, now_ms, &rows)
                        .expect("compute hour snapshot");
                assert_eq!(point.window_start_ms, window.start_ms);
                assert_eq!(point.window_end_ms, window.end_ms);
                assert!(point.margin_new_create.p50_ms.is_some());
                assert!(point.futures_new_create.p50_ms.is_some());
                store.upsert(point);
            }
            let loaded = store.load(slug, HOUR_MS * 10, HOUR_MS * 12);
            assert_eq!(loaded.strategy_slug, slug);
            assert_eq!(loaded.points.len(), 2);
            assert_eq!(loaded.points[0].window_start_ms, HOUR_MS * 10);
            assert_eq!(loaded.points[1].window_start_ms, HOUR_MS * 11);
            let default_windows =
                planned_hour_windows(now_ms, None, None).expect("default last hour");
            assert_eq!(default_windows.len(), 1);
            assert_eq!(default_windows[0].start_ms, HOUR_MS * 11);
            let latest = store.load(slug, default_windows[0].start_ms, default_windows[0].end_ms);
            assert_eq!(latest.points.len(), 1);
            assert_eq!(latest.points[0].window_start_ms, HOUR_MS * 11);
        }
    }
}
