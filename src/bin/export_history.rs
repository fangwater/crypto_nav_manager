#[path = "export_history/model.rs"]
mod model;
#[path = "export_history/query.rs"]
mod query;
#[path = "export_history/storage.rs"]
mod storage;
#[path = "export_history/write.rs"]
mod write;

use anyhow::{Context, Result, bail};
use clap::Parser;
use model::{Dataset, Strategy, selected_datasets, strategy_output_dir, validate_time_range};
use query::{load_cash_rows, load_trade_rows};
use sqlx::PgPool;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};
use storage::{cash_storage, connect_postgres, load_strategy, trade_storage};
use write::{write_cash_csv, write_trade_csv};

#[derive(Debug, Parser)]
#[command(about = "Export normalized strategy history from PostgreSQL to daily CSV files")]
struct Args {
    /// Strategy slug registered in strategy_envs. May be repeated.
    #[arg(long, required = true)]
    strategy: Vec<String>,

    /// Dataset to export.
    #[arg(long, value_enum, default_value_t = Dataset::All)]
    dataset: Dataset,

    /// Root directory. Each strategy is written to its own child directory.
    #[arg(long, default_value = "data")]
    output_dir: PathBuf,

    /// Inclusive minimum event timestamp.
    #[arg(long)]
    start_ms: Option<i64>,

    /// Inclusive maximum event timestamp.
    #[arg(long)]
    end_ms: Option<i64>,

    /// Overrides CRYPTO_NAV_DATABASE_URL.
    #[arg(long)]
    database_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    validate_time_range(args.start_ms, args.end_ms)?;
    fs::create_dir_all(&args.output_dir)
        .with_context(|| format!("create output directory {}", args.output_dir.display()))?;

    let pool = connect_postgres(args.database_url.as_deref()).await?;
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .context("run PostgreSQL migrations")?;

    let mut slugs = BTreeSet::new();
    slugs.extend(args.strategy);
    for slug in slugs {
        let strategy = load_strategy(&pool, &slug).await?;
        let strategy_dir = strategy_output_dir(&args.output_dir, &strategy.slug)?;
        fs::create_dir_all(&strategy_dir)
            .with_context(|| format!("create strategy directory {}", strategy_dir.display()))?;

        println!(
            "\n{}: exchange={}, directory={}",
            strategy.slug,
            strategy.exchange,
            strategy_dir.display()
        );
        for dataset in selected_datasets(args.dataset) {
            if !strategy.supports(dataset) {
                if args.dataset == dataset {
                    bail!(
                        "{} {:?} does not support the {} dataset",
                        strategy.exchange,
                        strategy.class,
                        dataset.name()
                    );
                }
                println!("skip interest: disabled for this strategy class");
                continue;
            }

            let (rows, files) = export_dataset(
                &pool,
                &strategy,
                dataset,
                &strategy_dir,
                args.start_ms,
                args.end_ms,
            )
            .await
            .with_context(|| format!("export {} {}", strategy.slug, dataset.name()))?;
            println!("{} complete: rows={rows}, files={files}", dataset.name());
        }
    }

    pool.close().await;
    Ok(())
}

async fn export_dataset(
    pool: &PgPool,
    strategy: &Strategy,
    dataset: Dataset,
    output_dir: &Path,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
) -> Result<(usize, usize)> {
    match dataset {
        Dataset::Trades => {
            let storage = trade_storage(pool, &strategy.schema).await?;
            let rows = load_trade_rows(pool, strategy, storage, start_ms, end_ms).await?;
            let files = write_trade_csv(output_dir, &rows)?;
            Ok((rows.len(), files))
        }
        Dataset::Funding | Dataset::Interest => {
            let storage = cash_storage(pool, &strategy.schema, dataset).await?;
            let rows =
                load_cash_rows(pool, &strategy.schema, dataset, storage, start_ms, end_ms).await?;
            let files = write_cash_csv(output_dir, strategy, dataset, &rows)?;
            Ok((rows.len(), files))
        }
        Dataset::All => unreachable!(),
    }
}
