//! Capture frozen normalized inputs without starting NAV, running migrations,
//! querying an exchange, or changing catalogs. Files contain private positions;
//! they belong in an operator-only directory, never in Git.
use super::*;
use clap::Parser;
use serde::{Deserialize, Serialize};
use std::{io::Write, os::unix::fs::OpenOptionsExt, path::PathBuf};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    end_ms: i64,
    #[arg(long)]
    account: Vec<String>,
    #[arg(long)]
    ipc_marks: bool,
}

#[derive(Serialize, Deserialize)]
struct Fixture {
    account: String,
    context: String,
    source: PnlSourceKind,
    inputs: PnlInputs,
    start: i64,
    end: i64,
    boundaries: Vec<i64>,
}

#[derive(sqlx::FromRow)]
struct Account {
    slug: String,
    db_schema: String,
    st_ms: i64,
    strategy_kind: String,
    exchange: String,
    account_mode: String,
}

pub async fn capture() -> Result<()> {
    let args = Args::parse();
    anyhow::ensure!(args.end_ms > 0, "end-ms must be positive");
    let pool = crate::postgres::pool_options(1, true)
        .connect_with(crate::server::postgres_options()?)
        .await?;
    let accounts=sqlx::query_as::<_,Account>("SELECT slug,db_schema,st_ms,strategy_kind,exchange,account_mode FROM strategy_envs WHERE enabled AND (cardinality($1::text[])=0 OR slug=ANY($1)) AND (strategy_kind='funding_rate' OR (strategy_kind='market_making' AND slug IN ('okex_mm_83777','bybit_mm_alpha'))) ORDER BY slug")
        .bind(&args.account).fetch_all(&pool).await?;
    let marks = if args.ipc_marks {
        let cache = MarkPriceCache::ipc_only();
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        cache
    } else {
        MarkPriceCache::default()
    }; // Never performs exchange HTTP.
    let mut fixtures = Vec::new();
    for account in accounts {
        let Some(source) = PnlSourceKind::for_strategy(
            &account.strategy_kind,
            &account.exchange,
            &account.account_mode,
        ) else {
            continue;
        };
        let initial=sqlx::query_as::<_,(i64,serde_json::Value)>("SELECT s.snapshot_ts_ms,s.payload FROM strategy_initial_snapshots i JOIN strategy_snapshots s ON s.strategy_slug=i.strategy_slug AND s.snapshot_ts_ms=i.snapshot_ts_ms WHERE i.strategy_slug=$1")
            .bind(&account.slug).fetch_optional(&pool).await?;
        let start = initial.as_ref().map_or(account.st_ms, |s| s.0);
        let mut inputs = load_inputs(
            &pool,
            source,
            &account.db_schema,
            &account.exchange,
            &marks,
            if initial.is_some() { start + 1 } else { start },
            args.end_ms,
        )
        .await?;
        if let Some((_, payload)) = initial {
            let initial = crate::server::parse_initial_positions(&payload)?;
            if initial.skipped_count != 0 {
                eprintln!(
                    "account={} unavailable=unpriced initial positions",
                    account.slug
                );
                continue;
            }
            inputs.initial_positions = initial.positions;
        }
        let mut boundaries = vec![start, args.end_ms];
        let interval = 4 * 3_600_000;
        let mut tick = (start.div_euclid(interval) + 1) * interval;
        while tick < args.end_ms {
            boundaries.push(tick);
            tick += interval;
        }
        fixtures.push(Fixture {
            account: account.slug,
            context: format!(
                "{}:{}:{}:{}",
                account.db_schema, account.exchange, account.st_ms, start
            ),
            source,
            inputs,
            start: args.end_ms - 46 * DAY_MS,
            end: args.end_ms,
            boundaries,
        });
    }
    anyhow::ensure!(!fixtures.is_empty(), "no matching supported accounts");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&args.output)?;
    let mut output = std::io::BufWriter::new(file);
    serde_json::to_writer(&mut output, &fixtures)?;
    output.flush()?;
    println!(
        "captured {} accounts at {} without exchange access",
        fixtures.len(),
        args.end_ms
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "operator-owned normalized input capture required"]
    fn audit_fixtures_against_original_lot_scan_and_incremental_replay() {
        let path = std::env::var("NAV_PNL_AUDIT_FIXTURE").expect("set NAV_PNL_AUDIT_FIXTURE");
        let fixtures: Vec<Fixture> =
            serde_json::from_reader(std::io::BufReader::new(std::fs::File::open(path).unwrap()))
                .unwrap();
        let mut checked = 0;
        for f in fixtures {
            let started = std::time::Instant::now();
            let legacy = crate::fifo_valuation::with_reference(|| {
                overview::replay(f.source, f.inputs.clone(), f.start, f.end, &f.boundaries)
            });
            let reference_ms = started.elapsed().as_millis();
            let legacy = match legacy {
                Ok(v) => v,
                Err(e) => {
                    let optimized =
                        overview::replay(f.source, f.inputs.clone(), f.start, f.end, &f.boundaries)
                            .unwrap_err();
                    assert_eq!(
                        e.to_string(),
                        optimized.to_string(),
                        "{} availability",
                        f.account
                    );
                    let mut cache = overview::ReplayCache::default();
                    assert!(
                        cache
                            .replay(
                                &f.account,
                                &f.context,
                                f.source,
                                f.inputs,
                                f.start,
                                f.end,
                                &f.boundaries
                            )
                            .is_err()
                    );
                    println!("account={} unavailable={e}", f.account);
                    continue;
                }
            };
            let started = std::time::Instant::now();
            let full = overview::replay(f.source, f.inputs.clone(), f.start, f.end, &f.boundaries)
                .unwrap();
            let optimized_ms = started.elapsed().as_millis();
            assert_eq!(
                legacy.realized_boundaries, full.realized_boundaries,
                "{} realized boundaries",
                f.account
            );
            assert_eq!(
                legacy.final_realized_net, full.final_realized_net,
                "{} realized total",
                f.account
            );
            assert_eq!(legacy.points.len(), full.points.len());
            let mut max_delta = 0.0_f64;
            for ((at, a), (bt, b)) in legacy.points.iter().zip(&full.points) {
                assert_eq!(at, bt);
                let delta = (a - b).abs();
                max_delta = max_delta.max(delta);
                assert!(
                    delta <= 1e-7 + a.abs() * 1e-11,
                    "{} at={at} legacy={a} optimized={b} delta={delta}",
                    f.account
                );
            }
            for (ts, a) in &legacy.boundaries {
                let b = full.boundaries[ts];
                assert!((a - b).abs() <= 1e-7 + a.abs() * 1e-11);
            }
            let mut cache = overview::ReplayCache::default();
            let prior_end = f.end - 300_000;
            let prior_bounds = f
                .boundaries
                .iter()
                .copied()
                .filter(|ts| *ts <= prior_end)
                .collect::<Vec<_>>();
            cache
                .replay(
                    &f.account,
                    &f.context,
                    f.source,
                    f.inputs.clone(),
                    f.start - 300_000,
                    prior_end,
                    &prior_bounds,
                )
                .unwrap();
            let started = std::time::Instant::now();
            let (warm, stats) = cache
                .replay(
                    &f.account,
                    &f.context,
                    f.source,
                    f.inputs,
                    f.start,
                    f.end,
                    &f.boundaries,
                )
                .unwrap();
            let warm_ms = started.elapsed().as_millis();
            assert_eq!(full, warm, "{} warm replay", f.account);
            println!(
                "account={} events={} reused={} replayed={} reference_ms={reference_ms} optimized_ms={optimized_ms} warm_ms={warm_ms} max_delta={max_delta:.12}",
                f.account, stats.total_events, stats.reused_events, stats.replayed_events
            );
            checked += 1;
        }
        assert!(checked > 0, "no fully priced fixtures audited");
    }
}
