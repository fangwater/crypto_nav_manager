use sqlx::PgPool;
use std::net::IpAddr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RestIpPoolError {
    #[error("query REST egress IPs for exchange {exchange}: {source}")]
    Query {
        exchange: String,
        #[source]
        source: sqlx::Error,
    },
    #[error("invalid REST egress IP in PostgreSQL: {value}")]
    InvalidIp {
        value: String,
        #[source]
        source: std::net::AddrParseError,
    },
    #[error("no REST egress IP avoids exchange {exchange}")]
    NoAvailableIp { exchange: String },
}

/// Returns enabled local IPs which are not used by an env on the target
/// exchange. A process name or PID is never part of this decision.
pub async fn exchange_local_ips(
    pool: &PgPool,
    exchange: &str,
) -> Result<Vec<IpAddr>, RestIpPoolError> {
    let exchange = exchange.trim().to_ascii_lowercase();
    let rows: Vec<String> = sqlx::query_scalar(
        r#"
        SELECT host(candidate.ip)
        FROM rest_egress_ips AS candidate
        WHERE candidate.enabled
          AND NOT EXISTS (
              SELECT 1
              FROM rest_egress_ip_envs AS usage
              WHERE usage.ip = candidate.ip
                AND usage.exchange = $1
          )
        ORDER BY candidate.ip
        "#,
    )
    .bind(&exchange)
    .fetch_all(pool)
    .await
    .map_err(|source| RestIpPoolError::Query {
        exchange: exchange.clone(),
        source,
    })?;

    let ips = rows
        .into_iter()
        .map(|value| {
            value
                .parse()
                .map_err(|source| RestIpPoolError::InvalidIp { value, source })
        })
        .collect::<Result<Vec<_>, _>>()?;
    if ips.is_empty() {
        return Err(RestIpPoolError::NoAvailableIp { exchange });
    }
    Ok(ips)
}

pub async fn configured_or_exchange_local_ips(
    pool: &PgPool,
    exchange: &str,
    configured: Vec<IpAddr>,
) -> Result<Vec<IpAddr>, RestIpPoolError> {
    if configured.is_empty() {
        exchange_local_ips(pool, exchange).await
    } else {
        Ok(configured)
    }
}
