use clap::Parser;
use crypto_nav_manager::rest_dispatcher::{Dispatcher, DispatcherConfig, RequestSpec};
use reqwest::{Method, header::HeaderMap};
use std::{net::IpAddr, time::Duration};

#[derive(Debug, Parser)]
#[command(about = "Send one REST request through a rate-aware source-IP pool")]
struct Args {
    /// Source IPs configured on this host. Repeat or comma-separate the option.
    #[arg(long, required = true, value_delimiter = ',')]
    local_ip: Vec<IpAddr>,

    #[arg(long, default_value = "GET")]
    method: Method,

    /// Request header in `name:value` form. May be repeated.
    #[arg(long = "header", value_parser = parse_header)]
    headers: Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>,

    #[arg(long)]
    body: Option<String>,

    #[arg(long, default_value_t = 1)]
    weight: u32,

    #[arg(long, default_value_t = 1_200)]
    max_weight_per_minute: u32,

    /// Number of alternate-IP retries after HTTP 429/418.
    #[arg(long, default_value_t = 1)]
    rate_limit_retries: usize,

    /// Absolute used-weight response header, such as x-mbx-used-weight-1m.
    #[arg(long)]
    observed_weight_header: Option<reqwest::header::HeaderName>,

    url: String,
}

fn parse_header(
    value: &str,
) -> Result<(reqwest::header::HeaderName, reqwest::header::HeaderValue), String> {
    let (name, value) = value
        .split_once(':')
        .ok_or_else(|| "header must use name:value syntax".to_string())?;
    let name = name
        .trim()
        .parse()
        .map_err(|error| format!("invalid header name: {error}"))?;
    let value = value
        .trim()
        .parse()
        .map_err(|error| format!("invalid header value: {error}"))?;
    Ok((name, value))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let config = DispatcherConfig {
        local_ips: args.local_ip,
        max_weight_per_window: args.max_weight_per_minute,
        window: Duration::from_secs(60),
        max_rate_limit_retries: args.rate_limit_retries,
        observed_weight_header: args.observed_weight_header,
        ..DispatcherConfig::default()
    };
    let dispatcher = Dispatcher::new(config)?;

    let mut headers = HeaderMap::new();
    for (name, value) in args.headers {
        headers.append(name, value);
    }
    let request = RequestSpec {
        method: args.method,
        url: args.url,
        headers,
        body: args.body.map(String::into_bytes),
        weight: args.weight,
    };

    let result = dispatcher.dispatch(request).await?;
    let status = result.response.status();
    let body = result.response.text().await?;
    println!(
        "local_ip={} attempts={} status={}\n{}",
        result.local_ip, result.attempts, status, body
    );
    Ok(())
}
