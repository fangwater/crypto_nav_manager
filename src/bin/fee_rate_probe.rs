use clap::{Parser, ValueEnum};
use crypto_nav_manager::{
    exchange::{
        binance::{BinanceAccountMode, BinanceClient, BinanceCredentials},
        bitget::{BitgetClient, BitgetCredentials},
        gate::{GateClient, GateCredentials, GateFeeMarket},
        okx::{OkxClient, OkxCredentials, OkxInstrumentType},
    },
    fee_rate_store::store_trading_fee_rates,
    models::{ProductCategory, TradingFeeRate},
    rest_dispatcher::{Dispatcher, DispatcherConfig},
};
use serde_json::Value;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::{env, net::IpAddr};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Exchange {
    BinanceUsdm,
    BinancePortfolioMargin,
    Gate,
    Bitget,
    Okx,
}

#[derive(Debug, Parser)]
#[command(about = "Probe one exchange account's normalized trading fee rates")]
struct Args {
    #[arg(long, value_enum)]
    exchange: Exchange,

    #[arg(long)]
    local_ip: IpAddr,

    #[arg(long, default_value = "BTCUSDT")]
    symbol: String,

    #[arg(long, default_value = "BTC_USDT")]
    gate_currency_pair: String,

    #[arg(long, default_value = "usdt")]
    gate_settle: String,

    #[arg(long, default_value = "BTC-USDT")]
    okx_instrument_family: String,

    /// Print the exchange response without shared normalization.
    #[arg(long)]
    raw: bool,

    /// Optionally persist normalized rows to this strategy schema.
    #[arg(long, conflicts_with = "raw")]
    db_schema: Option<String>,
}

fn required_env(name: &'static str) -> Result<String, Box<dyn std::error::Error>> {
    env::var(name).map_err(|_| format!("required environment variable {name} is not set").into())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let dispatcher = Dispatcher::new(DispatcherConfig {
        local_ips: vec![args.local_ip],
        ..DispatcherConfig::default()
    })?;

    let value = match args.exchange {
        Exchange::BinanceUsdm | Exchange::BinancePortfolioMargin => {
            let mode = match args.exchange {
                Exchange::BinanceUsdm => BinanceAccountMode::UsdmFutures,
                Exchange::BinancePortfolioMargin => BinanceAccountMode::PortfolioMargin,
                _ => unreachable!(),
            };
            let client = BinanceClient::new(
                dispatcher,
                BinanceCredentials::new(
                    required_env("BINANCE_API_KEY")?,
                    required_env("BINANCE_API_SECRET")?,
                ),
                mode,
            );
            if args.raw {
                client.raw_fee_rates(&args.symbol).await?
            } else {
                serde_json::to_value(client.fee_rates(&args.symbol).await?)?
            }
        }
        Exchange::Gate => {
            let client = GateClient::new(
                dispatcher,
                GateCredentials::new(
                    required_env("GATE_API_KEY")?,
                    required_env("GATE_API_SECRET")?,
                ),
            );
            if args.raw {
                client
                    .raw_fee_rates(GateFeeMarket::UsdtFutures, &args.gate_currency_pair)
                    .await?
            } else {
                serde_json::to_value(
                    client
                        .fee_rates(GateFeeMarket::UsdtFutures, &args.gate_currency_pair)
                        .await?,
                )?
            }
        }
        Exchange::Bitget => {
            let client = BitgetClient::new(
                dispatcher,
                BitgetCredentials::new(
                    required_env("BITGET_API_KEY")?,
                    required_env("BITGET_API_SECRET")?,
                    required_env("BITGET_API_PASSPHRASE")?,
                ),
            );
            if args.raw {
                Value::Array(
                    client
                        .raw_fee_rates(ProductCategory::UsdtFutures, &args.symbol)
                        .await?,
                )
            } else {
                serde_json::to_value(
                    client
                        .fee_rates(ProductCategory::UsdtFutures, &args.symbol)
                        .await?,
                )?
            }
        }
        Exchange::Okx => {
            let client = OkxClient::new(
                dispatcher,
                OkxCredentials::new(
                    required_env("OKX_API_KEY")?,
                    required_env("OKX_API_SECRET")?,
                    required_env("OKX_PASSPHRASE")?,
                ),
            );
            if args.raw {
                Value::Array(
                    client
                        .raw_fee_rates(OkxInstrumentType::Swap, &args.okx_instrument_family)
                        .await?,
                )
            } else {
                serde_json::to_value(
                    client
                        .fee_rates(OkxInstrumentType::Swap, &args.okx_instrument_family)
                        .await?,
                )?
            }
        }
    };

    if let Some(schema) = &args.db_schema {
        let rates: Vec<TradingFeeRate> = serde_json::from_value(value.clone())?;
        let options = match env::var("CRYPTO_NAV_DATABASE_URL") {
            Ok(url) => url.parse::<PgConnectOptions>()?,
            Err(env::VarError::NotPresent) => PgConnectOptions::new()
                .host("/var/run/postgresql")
                .username("ubuntu")
                .database("crypto_nav_manager"),
            Err(error) => return Err(error.into()),
        };
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        let stored = store_trading_fee_rates(&pool, schema, &rates).await?;
        pool.close().await;
        eprintln!("stored_rows={stored} schema={schema}");
    }

    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}
