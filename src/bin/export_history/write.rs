use super::model::{CashCsvRow, CashRow, Dataset, Strategy, TradeCsvRow, stable_asset};
use anyhow::{Context, Result};
use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use std::{collections::BTreeMap, path::Path};

pub(crate) fn write_trade_csv(output_dir: &Path, rows: &[TradeCsvRow]) -> Result<usize> {
    let daily = group_by_day(rows, |row| row.ts)?;
    for (date, day_rows) in &daily {
        let path = output_dir.join(format!("trades_{date}.csv"));
        let mut writer = csv::Writer::from_path(&path)
            .with_context(|| format!("create trade CSV {}", path.display()))?;
        for row in day_rows {
            writer
                .serialize(row)
                .with_context(|| format!("write trade CSV {}", path.display()))?;
        }
        writer
            .flush()
            .with_context(|| format!("flush trade CSV {}", path.display()))?;
    }
    Ok(daily.len())
}

pub(crate) fn write_cash_csv(
    output_dir: &Path,
    strategy: &Strategy,
    dataset: Dataset,
    rows: &[CashRow],
) -> Result<usize> {
    let daily = group_by_day(rows, |row| row.event_time_ms)?;
    let beijing = FixedOffset::east_opt(8 * 60 * 60).expect("UTC+8 is valid");
    for (date, day_rows) in &daily {
        let path = output_dir.join(format!("{}_{date}.csv", dataset.name()));
        let mut writer = csv::Writer::from_path(&path)
            .with_context(|| format!("create {} CSV {}", dataset.name(), path.display()))?;
        for row in day_rows {
            let timestamp = DateTime::<Utc>::from_timestamp_millis(row.event_time_ms)
                .expect("cash timestamps were validated while loading");
            let amount_usdt = row
                .amount_usdt
                .as_deref()
                .or_else(|| stable_asset(&row.asset).then_some(row.amount.as_str()))
                .unwrap_or("");
            writer
                .serialize(CashCsvRow {
                    exchange: &strategy.exchange,
                    account: &strategy.account,
                    symbol: row.symbol.as_deref().unwrap_or(""),
                    asset: &row.asset,
                    amount: &row.amount,
                    amountu: amount_usdt,
                    row_type: match dataset {
                        Dataset::Funding => "FUNDING_FEE",
                        Dataset::Interest => "INTEREST",
                        _ => unreachable!(),
                    },
                    record_id: &row.record_id,
                    ts: row.event_time_ms,
                    dt_utc: timestamp.to_rfc3339_opts(SecondsFormat::Secs, false),
                    dt_bj: timestamp
                        .with_timezone(&beijing)
                        .format("%Y-%m-%dT%H:%M:%S")
                        .to_string(),
                    raw: &row.raw,
                })
                .with_context(|| format!("write {} CSV {}", dataset.name(), path.display()))?;
        }
        writer
            .flush()
            .with_context(|| format!("flush {} CSV {}", dataset.name(), path.display()))?;
    }
    Ok(daily.len())
}

fn group_by_day<T, F>(rows: &[T], timestamp: F) -> Result<BTreeMap<String, Vec<&T>>>
where
    F: Fn(&T) -> i64,
{
    let mut daily = BTreeMap::<String, Vec<&T>>::new();
    for row in rows {
        let value = timestamp(row);
        let date = DateTime::<Utc>::from_timestamp_millis(value)
            .with_context(|| format!("invalid export timestamp {value}"))?
            .format("%Y-%m-%d")
            .to_string();
        daily.entry(date).or_default().push(row);
    }
    Ok(daily)
}
