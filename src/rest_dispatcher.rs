use reqwest::{
    Client, Method, Response,
    header::{HeaderMap, HeaderName, RETRY_AFTER},
};
use std::{
    net::IpAddr,
    sync::Arc,
    time::{Duration, Instant},
};
use thiserror::Error;
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct DispatcherConfig {
    pub local_ips: Vec<IpAddr>,
    pub max_weight_per_window: u32,
    pub window: Duration,
    pub request_timeout: Duration,
    pub tcp_keepalive: Option<Duration>,
    pub cooldown_429: Duration,
    pub backoff_418: Duration,
    pub max_rate_limit_retries: usize,
    /// Optional response header containing the IP's absolute used weight in the
    /// current server-side window, for example `x-mbx-used-weight-1m`.
    pub observed_weight_header: Option<HeaderName>,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            local_ips: Vec::new(),
            max_weight_per_window: 1_200,
            window: Duration::from_secs(60),
            request_timeout: Duration::from_secs(10),
            tcp_keepalive: Some(Duration::from_secs(30)),
            cooldown_429: Duration::from_secs(60),
            backoff_418: Duration::from_secs(120),
            max_rate_limit_retries: 1,
            observed_weight_header: None,
        }
    }
}

impl DispatcherConfig {
    fn validate(&self) -> Result<(), DispatchError> {
        if self.local_ips.is_empty() {
            return Err(DispatchError::InvalidConfig(
                "at least one local IP is required".to_string(),
            ));
        }
        if self.max_weight_per_window == 0 {
            return Err(DispatchError::InvalidConfig(
                "max_weight_per_window must be greater than zero".to_string(),
            ));
        }
        if self.window.is_zero() {
            return Err(DispatchError::InvalidConfig(
                "window must be greater than zero".to_string(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct RequestSpec {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    pub body: Option<Vec<u8>>,
    /// Weight charged against the selected IP's local quota.
    pub weight: u32,
}

impl RequestSpec {
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Self {
            method,
            url: url.into(),
            headers: HeaderMap::new(),
            body: None,
            weight: 1,
        }
    }
}

#[derive(Debug)]
pub struct DispatchResponse {
    pub local_ip: IpAddr,
    pub attempts: usize,
    pub response: Response,
}

#[derive(Debug, Error)]
pub enum DispatchError {
    #[error("invalid dispatcher configuration: {0}")]
    InvalidConfig(String),
    #[error("request weight {weight} exceeds per-IP window limit {limit}")]
    WeightExceedsLimit { weight: u32, limit: u32 },
    #[error("no source IP is currently available; retry after {retry_after:?}")]
    NoAvailableIp { retry_after: Option<Duration> },
    #[error("request through source IP {local_ip} failed: {source}")]
    Request {
        local_ip: IpAddr,
        #[source]
        source: reqwest::Error,
    },
    #[error(
        "all attempted source IPs were rate limited; last IP {local_ip}, status {status}, retry after {retry_after:?}"
    )]
    RateLimited {
        local_ip: IpAddr,
        status: reqwest::StatusCode,
        retry_after: Duration,
    },
}

#[derive(Debug)]
struct IpClient {
    ip: IpAddr,
    client: Client,
    used_weight: u32,
    window_started_at: Instant,
    blocked_until: Option<Instant>,
}

impl IpClient {
    fn refresh(&mut self, now: Instant, window: Duration) {
        if now.duration_since(self.window_started_at) >= window {
            self.used_weight = 0;
            self.window_started_at = now;
        }
        if self.blocked_until.is_some_and(|until| now >= until) {
            self.blocked_until = None;
        }
    }
}

#[derive(Debug)]
struct PoolState {
    clients: Vec<IpClient>,
    next_tie_break: usize,
}

#[derive(Clone, Debug)]
pub struct Dispatcher {
    config: Arc<DispatcherConfig>,
    state: Arc<Mutex<PoolState>>,
}

impl Dispatcher {
    pub fn new(config: DispatcherConfig) -> Result<Self, DispatchError> {
        config.validate()?;

        let mut clients = Vec::with_capacity(config.local_ips.len());
        for &ip in &config.local_ips {
            let client = Client::builder()
                .local_address(ip)
                .tcp_keepalive(config.tcp_keepalive)
                .timeout(config.request_timeout)
                .build()
                .map_err(|error| {
                    DispatchError::InvalidConfig(format!(
                        "failed to build client for {ip}: {error}"
                    ))
                })?;
            clients.push(IpClient {
                ip,
                client,
                used_weight: 0,
                window_started_at: Instant::now(),
                blocked_until: None,
            });
        }

        Ok(Self {
            config: Arc::new(config),
            state: Arc::new(Mutex::new(PoolState {
                clients,
                next_tie_break: 0,
            })),
        })
    }

    /// Dispatches once, retrying only explicit HTTP 429/418 responses on a
    /// different source IP. Transport errors are never retried because doing so
    /// could duplicate a non-idempotent order request.
    pub async fn dispatch(&self, request: RequestSpec) -> Result<DispatchResponse, DispatchError> {
        if request.weight > self.config.max_weight_per_window {
            return Err(DispatchError::WeightExceedsLimit {
                weight: request.weight,
                limit: self.config.max_weight_per_window,
            });
        }

        let max_attempts = self.config.max_rate_limit_retries.saturating_add(1);
        for attempt in 1..=max_attempts {
            let (index, local_ip, client) = self.reserve_client(request.weight).await?;
            let response = client
                .request(request.method.clone(), &request.url)
                .headers(request.headers.clone())
                .body(request.body.clone().unwrap_or_default())
                .send()
                .await
                .map_err(|source| DispatchError::Request { local_ip, source })?;

            self.update_observed_weight(index, response.headers()).await;

            let status = response.status();
            if status.as_u16() != 429 && status.as_u16() != 418 {
                return Ok(DispatchResponse {
                    local_ip,
                    attempts: attempt,
                    response,
                });
            }

            let retry_after = retry_after(response.headers()).unwrap_or_else(|| {
                if status.as_u16() == 418 {
                    self.config.backoff_418
                } else {
                    self.config.cooldown_429
                }
            });
            self.block_client(index, retry_after).await;

            if attempt == max_attempts {
                return Err(DispatchError::RateLimited {
                    local_ip,
                    status,
                    retry_after,
                });
            }
        }

        unreachable!("max_attempts is always at least one")
    }

    async fn reserve_client(&self, weight: u32) -> Result<(usize, IpAddr, Client), DispatchError> {
        let now = Instant::now();
        let mut state = self.state.lock().await;
        for client in &mut state.clients {
            client.refresh(now, self.config.window);
        }

        let len = state.clients.len();
        let start = state.next_tie_break % len;
        let selected = (0..len)
            .map(|offset| (start + offset) % len)
            .filter(|&index| {
                let client = &state.clients[index];
                client.blocked_until.is_none()
                    && client.used_weight.saturating_add(weight)
                        <= self.config.max_weight_per_window
            })
            .min_by_key(|&index| state.clients[index].used_weight);

        let Some(index) = selected else {
            let retry_after = state
                .clients
                .iter()
                .map(|client| {
                    let mut ready = now;
                    if let Some(blocked_until) = client.blocked_until {
                        ready = ready.max(blocked_until);
                    }
                    if client.used_weight.saturating_add(weight) > self.config.max_weight_per_window
                    {
                        ready = ready.max(client.window_started_at + self.config.window);
                    }
                    ready
                })
                .min()
                .map(|ready| ready.saturating_duration_since(now));
            return Err(DispatchError::NoAvailableIp { retry_after });
        };

        state.next_tie_break = (index + 1) % len;
        let selected = &mut state.clients[index];
        selected.used_weight = selected.used_weight.saturating_add(weight);
        Ok((index, selected.ip, selected.client.clone()))
    }

    async fn block_client(&self, index: usize, duration: Duration) {
        let mut state = self.state.lock().await;
        state.clients[index].blocked_until = Some(Instant::now() + duration);
    }

    async fn update_observed_weight(&self, index: usize, headers: &HeaderMap) {
        let Some(name) = &self.config.observed_weight_header else {
            return;
        };
        let Some(observed) = headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u32>().ok())
        else {
            return;
        };

        let mut state = self.state.lock().await;
        // Concurrent responses can arrive out of order, so never move the local
        // estimate backwards within a window.
        state.clients[index].used_weight = state.clients[index].used_weight.max(observed);
    }
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn config(local_ips: Vec<IpAddr>, limit: u32) -> DispatcherConfig {
        DispatcherConfig {
            local_ips,
            max_weight_per_window: limit,
            ..DispatcherConfig::default()
        }
    }

    #[tokio::test]
    async fn balances_equal_quota_across_ips() {
        let first = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let second = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let dispatcher = Dispatcher::new(config(vec![first, second], 10)).unwrap();

        let (_, selected_1, _) = dispatcher.reserve_client(1).await.unwrap();
        let (_, selected_2, _) = dispatcher.reserve_client(1).await.unwrap();

        assert_eq!(selected_1, first);
        assert_eq!(selected_2, second);
    }

    #[tokio::test]
    async fn rejects_when_all_ip_quotas_are_reserved() {
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let dispatcher = Dispatcher::new(config(vec![ip], 2)).unwrap();

        dispatcher.reserve_client(2).await.unwrap();
        let error = dispatcher.reserve_client(1).await.unwrap_err();

        assert!(matches!(error, DispatchError::NoAvailableIp { .. }));
    }

    #[tokio::test]
    async fn skips_a_blocked_ip() {
        let first = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let second = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));
        let dispatcher = Dispatcher::new(config(vec![first, second], 10)).unwrap();
        dispatcher.block_client(0, Duration::from_secs(10)).await;

        let (_, selected, _) = dispatcher.reserve_client(1).await.unwrap();

        assert_eq!(selected, second);
    }

    #[test]
    fn parses_retry_after_seconds() {
        let mut headers = HeaderMap::new();
        headers.insert(RETRY_AFTER, "17".parse().unwrap());

        assert_eq!(retry_after(&headers), Some(Duration::from_secs(17)));
    }
}
