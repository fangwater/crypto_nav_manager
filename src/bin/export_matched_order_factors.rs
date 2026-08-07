use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use clap::Parser;
use polars::prelude::*;
use reqwest::blocking::Client;
use std::{
    collections::BTreeSet,
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process,
    time::{Duration, UNIX_EPOCH},
};

const FACTOR_INTERVAL_US: i64 = 5_000_000;
const DEFAULT_ALIAS: &str = "binance-mt";
const DEFAULT_CLICKHOUSE_URL: &str = "http://127.0.0.1:8123";
const FACTOR_DATABASE: &str = "fusion";
const FACTOR_TABLE: &str = "fusion_factor_binance_futures_5s";

#[derive(Debug, Parser)]
#[command(about = "Export order-aligned, backward-safe factor Parquet files")]
struct Args {
    /// Matched-order directory alias. May be repeated. Defaults to binance-mt.
    #[arg(long)]
    alias: Vec<String>,

    /// UTC trade date in YYYYMMDD form. May be repeated. Defaults to every source file.
    #[arg(long, value_parser = parse_trade_date)]
    trade_date: Vec<NaiveDate>,

    #[arg(long)]
    output_root: Option<PathBuf>,

    /// Overrides CRYPTO_NAV_CLICKHOUSE_URL.
    #[arg(long)]
    clickhouse_url: Option<String>,

    /// Rebuild selected files even when source and factor states are current.
    #[arg(long)]
    force: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ExportState {
    source_size: u64,
    source_modified_ns: u128,
    factor_rows: u64,
    factor_max_ts_us: i64,
    factor_max_replay_version: u64,
}

#[derive(Debug)]
struct OrderKey {
    fkey: i64,
    symbol: String,
    cts: i64,
}

#[derive(Clone, Copy, Debug)]
struct MatchStats {
    rows: usize,
    matched: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let output_root =
        crypto_nav_manager::runtime::matched_output_root(args.output_root.as_deref())?;
    let clickhouse_url = args
        .clickhouse_url
        .or_else(|| env::var("CRYPTO_NAV_CLICKHOUSE_URL").ok())
        .unwrap_or_else(|| DEFAULT_CLICKHOUSE_URL.to_string());
    let client = Client::builder()
        .pool_max_idle_per_host(0)
        .timeout(Duration::from_secs(180))
        .build()
        .context("build ClickHouse HTTP client")?;
    let factor_columns = load_factor_columns(&client, &clickhouse_url)?;
    let factor_state = load_factor_state(&client, &clickhouse_url)?;
    let factor_max_date = chrono::DateTime::from_timestamp_micros(factor_state.1)
        .context("invalid maximum factor timestamp")?
        .date_naive();
    let aliases = if args.alias.is_empty() {
        vec![DEFAULT_ALIAS.to_string()]
    } else {
        args.alias
    };
    let selected_dates = args.trade_date.into_iter().collect::<BTreeSet<_>>();
    let mut files_written = 0usize;
    let mut files_current = 0usize;
    let mut files_outside_factor_range = 0usize;
    let mut rows_written = 0usize;

    for alias in aliases {
        validate_alias(&alias)?;
        let source_dir = output_root.join(&alias).join("matched_order");
        let output_dir = output_root.join(&alias).join("matched_order_factor");
        let state_dir = output_root
            .join(".export_state")
            .join(&alias)
            .join("matched_order_factor");
        fs::create_dir_all(&output_dir)
            .with_context(|| format!("create factor output directory {}", output_dir.display()))?;
        fs::create_dir_all(&state_dir)
            .with_context(|| format!("create factor state directory {}", state_dir.display()))?;

        for (date, source_path) in source_files(&source_dir, &selected_dates)? {
            if date > factor_max_date {
                files_outside_factor_range += 1;
                continue;
            }
            let file_name = format!("{}.parquet", date.format("%Y%m%d"));
            let output_path = output_dir.join(&file_name);
            let state_path = state_dir.join(format!("{}.state", date.format("%Y%m%d")));
            let state = source_state(&source_path, factor_state)?;
            if !args.force
                && output_path.is_file()
                && read_export_state(&state_path)? == Some(state)
            {
                files_current += 1;
                continue;
            }

            let keys = read_order_keys(&source_path)?;
            let match_stats = validate_factor_selection(&client, &clickhouse_url, &keys)?;
            let parquet = fetch_factor_parquet(&client, &clickhouse_url, &factor_columns, &keys)?;
            validate_parquet_envelope(&parquet)?;
            write_bytes_atomic(&output_path, &parquet)?;
            write_export_state_atomic(&state_path, state)?;
            files_written += 1;
            rows_written += keys.len();
            println!(
                "exported matched_order_factor alias={} trade_date={} rows={} matched={} path={}",
                alias,
                date.format("%Y%m%d"),
                match_stats.rows,
                match_stats.matched,
                output_path.display()
            );
        }
    }

    println!(
        "matched_order_factor export complete: files_written={} files_current={} files_outside_factor_range={} rows_written={}",
        files_written, files_current, files_outside_factor_range, rows_written
    );
    Ok(())
}

fn parse_trade_date(value: &str) -> std::result::Result<NaiveDate, String> {
    NaiveDate::parse_from_str(value, "%Y%m%d")
        .map_err(|_| format!("invalid trade date {value:?}; expected YYYYMMDD"))
}

fn validate_alias(alias: &str) -> Result<()> {
    if alias.is_empty()
        || !alias
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("invalid matched-order alias {alias:?}");
    }
    Ok(())
}

fn source_files(
    source_dir: &Path,
    selected_dates: &BTreeSet<NaiveDate>,
) -> Result<Vec<(NaiveDate, PathBuf)>> {
    if !source_dir.is_dir() {
        bail!(
            "matched-order source directory does not exist: {}",
            source_dir.display()
        );
    }
    if !selected_dates.is_empty() {
        return selected_dates
            .iter()
            .map(|date| {
                let path = source_dir.join(format!("{}.parquet", date.format("%Y%m%d")));
                if !path.is_file() {
                    bail!(
                        "matched-order source file does not exist: {}",
                        path.display()
                    );
                }
                Ok((*date, path))
            })
            .collect();
    }

    let mut files = Vec::new();
    for entry in fs::read_dir(source_dir)
        .with_context(|| format!("read matched-order directory {}", source_dir.display()))?
    {
        let path = entry?.path();
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if path.extension().and_then(|value| value.to_str()) != Some("parquet") {
            continue;
        }
        let Ok(date) = NaiveDate::parse_from_str(stem, "%Y%m%d") else {
            continue;
        };
        files.push((date, path));
    }
    files.sort_by_key(|(date, _)| *date);
    Ok(files)
}

fn load_factor_columns(client: &Client, clickhouse_url: &str) -> Result<Vec<String>> {
    let query = format!(
        "SELECT name FROM system.columns WHERE database='{FACTOR_DATABASE}' \
         AND table='{FACTOR_TABLE}' AND name NOT IN ('ts','symbol','replay_version') \
         ORDER BY position FORMAT TabSeparatedRaw"
    );
    let body = clickhouse_post(client, clickhouse_url, &query)?;
    let text = String::from_utf8(body).context("decode ClickHouse factor columns")?;
    let columns = text
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| {
            validate_identifier(name)?;
            Ok(name.to_string())
        })
        .collect::<Result<Vec<_>>>()?;
    if columns.is_empty() {
        bail!("ClickHouse factor table has no factor columns");
    }
    Ok(columns)
}

fn load_factor_state(client: &Client, clickhouse_url: &str) -> Result<(u64, i64, u64)> {
    let query = format!(
        "SELECT count(),toUnixTimestamp64Micro(max(ts)),max(replay_version) \
         FROM {FACTOR_DATABASE}.{FACTOR_TABLE} FORMAT TabSeparatedRaw"
    );
    let body = clickhouse_post(client, clickhouse_url, &query)?;
    let text = String::from_utf8(body).context("decode ClickHouse factor state")?;
    let mut fields = text.split_whitespace();
    let rows = fields.next().context("missing factor row count")?.parse()?;
    let max_ts_us = fields
        .next()
        .context("missing factor max timestamp")?
        .parse()?;
    let max_replay_version = fields
        .next()
        .context("missing factor replay version")?
        .parse()?;
    if fields.next().is_some() {
        bail!("unexpected ClickHouse factor state: {text:?}");
    }
    Ok((rows, max_ts_us, max_replay_version))
}

fn source_state(path: &Path, factor_state: (u64, i64, u64)) -> Result<ExportState> {
    let metadata = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    let source_modified_ns = metadata
        .modified()
        .with_context(|| format!("read modification time for {}", path.display()))?
        .duration_since(UNIX_EPOCH)
        .context("source modification time predates Unix epoch")?
        .as_nanos();
    Ok(ExportState {
        source_size: metadata.len(),
        source_modified_ns,
        factor_rows: factor_state.0,
        factor_max_ts_us: factor_state.1,
        factor_max_replay_version: factor_state.2,
    })
}

fn read_order_keys(path: &Path) -> Result<Vec<OrderKey>> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let frame = ParquetReader::new(file)
        .finish()
        .with_context(|| format!("read {}", path.display()))?;
    let fkeys = frame.column("fkey")?.i64()?;
    let symbols = frame.column("symbol")?.str()?;
    let timestamps = frame.column("cts")?.i64()?;
    let mut keys = Vec::with_capacity(frame.height());
    for index in 0..frame.height() {
        keys.push(OrderKey {
            fkey: fkeys.get(index).context("null order fkey")?,
            symbol: symbols.get(index).context("null order symbol")?.to_string(),
            cts: timestamps.get(index).context("null order cts")?,
        });
    }
    Ok(keys)
}

fn fetch_factor_parquet(
    client: &Client,
    clickhouse_url: &str,
    factor_columns: &[String],
    keys: &[OrderKey],
) -> Result<Vec<u8>> {
    let query = build_factor_query(factor_columns, keys);
    clickhouse_post(client, clickhouse_url, &query)
}

fn build_factor_query(factor_columns: &[String], keys: &[OrderKey]) -> String {
    let factor_select = factor_columns
        .iter()
        .map(|name| format!("f.{} AS {}", quote_identifier(name), quote_identifier(name)))
        .collect::<Vec<_>>()
        .join(",");
    let source_columns = factor_columns
        .iter()
        .map(|name| quote_identifier(name))
        .collect::<Vec<_>>()
        .join(",");
    let order_values = keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            format!(
                "({index},{},'{}',{},{})",
                key.fkey,
                escape_sql_string(&key.symbol),
                key.cts,
                factor_bucket_us(key.cts)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let order_table = if order_values.is_empty() {
        "(SELECT toUInt64(0) AS row_index,toInt64(0) AS fkey,\
         CAST('' AS String) AS symbol,toInt64(0) AS cts,\
         toInt64(0) AS lookup_ts_us WHERE 0)"
            .to_string()
    } else {
        format!(
            "VALUES('row_index UInt64,fkey Int64,symbol String,cts Int64,lookup_ts_us Int64',\
             {order_values})"
        )
    };
    let lookup_keys = keys
        .iter()
        .map(|key| {
            format!(
                "('{}',fromUnixTimestamp64Micro({},'UTC'))",
                escape_sql_string(&key.symbol),
                factor_bucket_us(key.cts)
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let factor_filter = if keys.is_empty() {
        "0".to_string()
    } else {
        format!("(symbol,ts) IN ({lookup_keys})")
    };
    format!(
        "SELECT o.fkey,o.symbol,o.cts,toUnixTimestamp64Micro(f.ts) AS factor_ts,\
         {factor_select} \
         FROM {order_table} AS o \
         LEFT JOIN (SELECT symbol,ts,{source_columns} \
         FROM {FACTOR_DATABASE}.{FACTOR_TABLE} FINAL WHERE {factor_filter}) AS f \
         ON o.symbol=f.symbol AND fromUnixTimestamp64Micro(o.lookup_ts_us,'UTC')=f.ts \
         ORDER BY o.row_index SETTINGS join_use_nulls=1 FORMAT Parquet"
    )
}

fn factor_bucket_us(cts: i64) -> i64 {
    cts.div_euclid(FACTOR_INTERVAL_US) * FACTOR_INTERVAL_US
}

fn validate_identifier(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        bail!("unsafe ClickHouse identifier {value:?}");
    }
    Ok(())
}

fn quote_identifier(value: &str) -> String {
    format!("`{value}`")
}

fn escape_sql_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn clickhouse_post(client: &Client, clickhouse_url: &str, query: &str) -> Result<Vec<u8>> {
    let mut url = reqwest::Url::parse(clickhouse_url).context("parse ClickHouse URL")?;
    url.query_pairs_mut()
        .append_pair("max_query_size", "16777216");
    let response = client
        .post(url)
        .header(reqwest::header::CONNECTION, "close")
        .body(query.to_string())
        .send()
        .context("send ClickHouse factor query")?;
    let status = response.status();
    let bytes = response.bytes().context("read ClickHouse response")?;
    if !status.is_success() {
        bail!(
            "ClickHouse factor query failed with {status}: {}",
            String::from_utf8_lossy(&bytes).trim()
        );
    }
    Ok(bytes.to_vec())
}

fn validate_factor_selection(
    client: &Client,
    clickhouse_url: &str,
    keys: &[OrderKey],
) -> Result<MatchStats> {
    if keys.is_empty() {
        return Ok(MatchStats {
            rows: 0,
            matched: 0,
        });
    }
    let order_values = keys
        .iter()
        .enumerate()
        .map(|(index, key)| {
            format!(
                "({index},{},'{}',{},{})",
                key.fkey,
                escape_sql_string(&key.symbol),
                key.cts,
                factor_bucket_us(key.cts)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let lookup_keys = keys
        .iter()
        .map(|key| {
            format!(
                "('{}',fromUnixTimestamp64Micro({},'UTC'))",
                escape_sql_string(&key.symbol),
                factor_bucket_us(key.cts)
            )
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        "SELECT count(),countIf(isNotNull(f.ts)),\
         countIf(toUnixTimestamp64Micro(f.ts)>o.cts),uniqExact(o.row_index) \
         FROM VALUES('row_index UInt64,fkey Int64,symbol String,cts Int64,lookup_ts_us Int64',\
         {order_values}) AS o \
         LEFT JOIN (SELECT symbol,ts FROM {FACTOR_DATABASE}.{FACTOR_TABLE} FINAL \
         WHERE (symbol,ts) IN ({lookup_keys})) AS f \
         ON o.symbol=f.symbol AND fromUnixTimestamp64Micro(o.lookup_ts_us,'UTC')=f.ts \
         SETTINGS join_use_nulls=1 FORMAT TabSeparatedRaw"
    );
    let body = clickhouse_post(client, clickhouse_url, &query)?;
    let text = String::from_utf8(body).context("decode factor match validation")?;
    let mut fields = text.split_whitespace();
    let rows: usize = fields
        .next()
        .context("missing validated row count")?
        .parse()?;
    let matched: usize = fields
        .next()
        .context("missing factor match count")?
        .parse()?;
    let future: usize = fields
        .next()
        .context("missing future factor count")?
        .parse()?;
    let unique_rows: usize = fields
        .next()
        .context("missing unique row-index count")?
        .parse()?;
    if fields.next().is_some() {
        bail!("unexpected factor validation response: {text:?}");
    }
    if rows != keys.len() || unique_rows != keys.len() {
        bail!(
            "factor selection row mismatch: expected={} rows={} unique_rows={}",
            keys.len(),
            rows,
            unique_rows
        );
    }
    if future != 0 {
        bail!("future factor validation failed: {future} rows");
    }
    Ok(MatchStats { rows, matched })
}

fn validate_parquet_envelope(parquet: &[u8]) -> Result<()> {
    if parquet.len() < 8 || &parquet[..4] != b"PAR1" || &parquet[parquet.len() - 4..] != b"PAR1" {
        bail!("ClickHouse response is not a complete Parquet file");
    }
    Ok(())
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("output path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .with_context(|| format!("invalid output file name: {}", path.display()))?;
    let temporary = parent.join(format!(".{file_name}.{}.tmp", process::id()));
    let result = (|| -> Result<()> {
        let mut file = File::create(&temporary)
            .with_context(|| format!("create temporary file {}", temporary.display()))?;
        file.write_all(bytes)
            .with_context(|| format!("write temporary file {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("sync temporary file {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("replace {} with {}", path.display(), temporary.display()))?;
        File::open(parent)
            .with_context(|| format!("open directory {}", parent.display()))?
            .sync_all()
            .with_context(|| format!("sync directory {}", parent.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn write_export_state_atomic(path: &Path, state: ExportState) -> Result<()> {
    let value = format!(
        "{} {} {} {} {}\n",
        state.source_size,
        state.source_modified_ns,
        state.factor_rows,
        state.factor_max_ts_us,
        state.factor_max_replay_version
    );
    write_bytes_atomic(path, value.as_bytes())
}

fn read_export_state(path: &Path) -> Result<Option<ExportState>> {
    let value = match fs::read_to_string(path) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut fields = value.split_whitespace();
    let Some(source_size) = fields.next().and_then(|value| value.parse().ok()) else {
        return Ok(None);
    };
    let Some(source_modified_ns) = fields.next().and_then(|value| value.parse().ok()) else {
        return Ok(None);
    };
    let Some(factor_rows) = fields.next().and_then(|value| value.parse().ok()) else {
        return Ok(None);
    };
    let Some(factor_max_ts_us) = fields.next().and_then(|value| value.parse().ok()) else {
        return Ok(None);
    };
    let Some(factor_max_replay_version) = fields.next().and_then(|value| value.parse().ok()) else {
        return Ok(None);
    };
    if fields.next().is_some() {
        return Ok(None);
    }
    Ok(Some(ExportState {
        source_size,
        source_modified_ns,
        factor_rows,
        factor_max_ts_us,
        factor_max_replay_version,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn factor_bucket_never_uses_the_future() {
        assert_eq!(
            factor_bucket_us(1_786_060_965_063_905),
            1_786_060_965_000_000
        );
        for cts in [0, 1, 4_999_999, 5_000_000, 5_000_001, i64::MAX] {
            let bucket = factor_bucket_us(cts);
            assert!(bucket <= cts);
            assert!(cts - bucket < FACTOR_INTERVAL_US);
        }
    }

    #[test]
    fn query_is_an_exact_left_join_with_ordered_output() {
        let keys = vec![OrderKey {
            fkey: 7,
            symbol: "BTCUSDT".to_string(),
            cts: 12_345_678,
        }];
        let query = build_factor_query(&["factor_001".to_string()], &keys);
        assert!(query.contains("LEFT JOIN"));
        assert!(query.contains("VALUES('row_index UInt64"));
        assert!(query.contains("(0,7,'BTCUSDT',12345678,10000000)"));
        assert!(query.contains("ORDER BY o.row_index"));
        assert!(!query.contains("ASOF"));
    }

    #[test]
    fn empty_query_uses_an_explicit_zero_row_source() {
        let query = build_factor_query(&["factor_001".to_string()], &[]);
        assert!(query.contains("FROM (SELECT toUInt64(0) AS row_index"));
        assert!(query.contains("lookup_ts_us WHERE 0) AS o"));
        assert!(!query.contains("lookup_ts_us Int64',)"));
    }

    #[test]
    fn rejects_unsafe_factor_identifiers() {
        assert!(validate_identifier("factor_001").is_ok());
        assert!(validate_identifier("factor`; DROP TABLE x").is_err());
    }
}
