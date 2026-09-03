use std::{
    collections::{BTreeMap, HashMap},
    env,
    io::{BufRead, BufReader, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, TrySendError, sync_channel},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tracing::{info, warn};

use crate::{
    config::{NotificationConfig, TargetConfig},
    model::{HealthStatus, Snapshot, SystemObservation, TargetObservation},
};

const NOTIFY_PATH: &str = "/v1/notify";
const API_TOKEN_ENV: &str = "PUBLIC_INFRA_NOTIFICATION_TOKEN";
const DELIVERY_ATTEMPTS: u32 = 3;

#[derive(Debug, Default)]
pub struct NotificationStats {
    enqueued: AtomicU64,
    accepted: AtomicU64,
    failed: AtomicU64,
    dropped: AtomicU64,
}

impl NotificationStats {
    pub fn snapshot(&self) -> NotificationStatsSnapshot {
        NotificationStatsSnapshot {
            enqueued: self.enqueued.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NotificationStatsSnapshot {
    pub enqueued: u64,
    pub accepted: u64,
    pub failed: u64,
    pub dropped: u64,
}

pub struct NotificationManager {
    config: NotificationConfig,
    sender: Option<SyncSender<NotifyRequest>>,
    stats: Arc<NotificationStats>,
    active: HashMap<String, ActiveAlert>,
    last_incident_closed_at: HashMap<String, u64>,
    no_established_alert_samples: HashMap<String, u32>,
}

impl NotificationManager {
    pub fn new(
        config: NotificationConfig,
        targets: &[TargetConfig],
    ) -> Result<(Self, Arc<NotificationStats>)> {
        let stats = Arc::new(NotificationStats::default());
        let sender = if config.enabled {
            let address = config.socket_address()?;
            let api_token = env::var(API_TOKEN_ENV)
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty());
            if api_token
                .as_deref()
                .is_some_and(|token| token.contains(['\r', '\n']))
            {
                bail!("{API_TOKEN_ENV} contains invalid characters");
            }
            let auth_configured = api_token.is_some();

            let (sender, receiver) = sync_channel(config.queue_capacity);
            let client = NotificationClient {
                address,
                timeout: Duration::from_millis(config.request_timeout_ms),
                api_token,
            };
            let worker_stats = Arc::clone(&stats);
            thread::Builder::new()
                .name("public-infra-notify".to_owned())
                .spawn(move || delivery_worker(receiver, client, worker_stats))
                .context("spawn notification delivery worker")?;
            info!(
                address = %address,
                queue_capacity = config.queue_capacity,
                auth = auth_configured,
                "local notification delivery enabled"
            );
            Some(sender)
        } else {
            None
        };

        let manager = Self {
            config,
            sender,
            stats: Arc::clone(&stats),
            active: HashMap::new(),
            last_incident_closed_at: HashMap::new(),
            no_established_alert_samples: targets
                .iter()
                .map(|target| (target.name.clone(), target.no_established_alert_samples))
                .collect(),
        };
        Ok((manager, stats))
    }

    pub fn observe(&mut self, snapshot: &Snapshot) {
        if self.sender.is_none() || snapshot.window_secs.is_none() {
            return;
        }

        for target in &snapshot.targets {
            let no_established_alert_samples = self
                .no_established_alert_samples
                .get(&target.name)
                .copied()
                .unwrap_or(1);
            let condition = target_condition(target, no_established_alert_samples);
            let key = format!("target:{}", target.name);
            if let Some(action) =
                self.next_state_action(&key, condition, snapshot.timestamp_unix_ms)
            {
                let request = target_state_request(snapshot, target, action);
                if self.enqueue(request) {
                    self.mark_state_action_delivered(&key, action, snapshot.timestamp_unix_ms);
                }
            }
        }

        let key = "system".to_owned();
        let condition = system_condition(&snapshot.system);
        if let Some(action) = self.next_state_action(&key, condition, snapshot.timestamp_unix_ms) {
            let request = system_state_request(snapshot, action);
            if self.enqueue(request) {
                self.mark_state_action_delivered(&key, action, snapshot.timestamp_unix_ms);
            }
        }
    }

    fn next_state_action(
        &mut self,
        key: &str,
        condition: AlertCondition,
        now_unix_ms: u64,
    ) -> Option<StateAction> {
        match condition {
            AlertCondition::Unknown => None,
            AlertCondition::Healthy => {
                if self.active.get(key)?.last_sent_at_unix_ms.is_none() {
                    self.active.remove(key);
                    return None;
                }
                let state = self.active.get_mut(key)?;
                state.healthy_samples = state.healthy_samples.saturating_add(1);
                (state.healthy_samples >= self.config.recovery_samples).then_some(
                    StateAction::Recovery {
                        opened_at_unix_ms: state.opened_at_unix_ms,
                    },
                )
            }
            AlertCondition::Alert { level, gate } => {
                let required_samples = gate.required_samples(self.config.alert_samples);
                let rearm_blocked = !gate.bypasses_rearm()
                    && self.last_incident_closed_at.get(key).is_some_and(|last| {
                        now_unix_ms.saturating_sub(*last)
                            < self.config.repeat_interval_secs.saturating_mul(1_000)
                    });
                let state = self.active.entry(key.to_owned()).or_insert(ActiveAlert {
                    level,
                    opened_at_unix_ms: now_unix_ms,
                    last_sent_at_unix_ms: None,
                    healthy_samples: 0,
                    fault_samples: 0,
                    gate,
                });
                if state.gate != gate {
                    state.gate = gate;
                    state.fault_samples = 0;
                    if state.last_sent_at_unix_ms.is_none() {
                        state.opened_at_unix_ms = now_unix_ms;
                    }
                }
                let escalated = state.last_sent_at_unix_ms.is_some()
                    && level_rank(level) > level_rank(state.level);
                if state.last_sent_at_unix_ms.is_none() {
                    state.level = level;
                }
                state.healthy_samples = 0;
                state.fault_samples = state.fault_samples.saturating_add(1);

                if state.fault_samples < required_samples {
                    return None;
                }

                let repeat_due = state.last_sent_at_unix_ms.is_some_and(|last| {
                    now_unix_ms.saturating_sub(last)
                        >= self.config.repeat_interval_secs.saturating_mul(1_000)
                });
                let initial_due = state.last_sent_at_unix_ms.is_none() && !rearm_blocked;
                if initial_due || escalated || repeat_due {
                    Some(StateAction::Alert {
                        level,
                        repeated: state.last_sent_at_unix_ms.is_some() && !escalated,
                    })
                } else {
                    None
                }
            }
        }
    }

    fn mark_state_action_delivered(&mut self, key: &str, action: StateAction, now_unix_ms: u64) {
        match action {
            StateAction::Alert { level, .. } => {
                if let Some(state) = self.active.get_mut(key) {
                    state.level = level;
                    state.last_sent_at_unix_ms = Some(now_unix_ms);
                }
            }
            StateAction::Recovery { .. } => {
                self.active.remove(key);
                self.last_incident_closed_at
                    .insert(key.to_owned(), now_unix_ms);
            }
        }
    }

    fn enqueue(&self, request: NotifyRequest) -> bool {
        let Some(sender) = &self.sender else {
            return false;
        };
        match sender.try_send(request) {
            Ok(()) => {
                self.stats.enqueued.fetch_add(1, Ordering::Relaxed);
                true
            }
            Err(TrySendError::Full(request)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                warn!(
                    dedup_key = request.dedup_key.as_deref().unwrap_or("none"),
                    "local notification queue is full; dropping event"
                );
                false
            }
            Err(TrySendError::Disconnected(request)) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
                warn!(
                    dedup_key = request.dedup_key.as_deref().unwrap_or("none"),
                    "local notification worker is unavailable; dropping event"
                );
                false
            }
        }
    }

    #[cfg(test)]
    fn test_instance(
        mut config: NotificationConfig,
        no_established_alert_samples: u32,
    ) -> (Self, Receiver<NotifyRequest>, Arc<NotificationStats>) {
        config.enabled = true;
        let stats = Arc::new(NotificationStats::default());
        let (sender, receiver) = sync_channel(config.queue_capacity);
        (
            Self {
                config,
                sender: Some(sender),
                stats: Arc::clone(&stats),
                active: HashMap::new(),
                last_incident_closed_at: HashMap::new(),
                no_established_alert_samples: HashMap::from([(
                    "spp_test".to_owned(),
                    no_established_alert_samples,
                )]),
            },
            receiver,
            stats,
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct ActiveAlert {
    level: AlertLevel,
    opened_at_unix_ms: u64,
    last_sent_at_unix_ms: Option<u64>,
    healthy_samples: u32,
    fault_samples: u32,
    gate: AlertGate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlertCondition {
    Unknown,
    Healthy,
    Alert { level: AlertLevel, gate: AlertGate },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlertGate {
    Immediate,
    Default,
    NoEstablished { required_samples: u32 },
}

impl AlertGate {
    const fn required_samples(self, default_samples: u32) -> u32 {
        match self {
            Self::Immediate => 1,
            Self::Default => default_samples,
            Self::NoEstablished { required_samples } => required_samples,
        }
    }

    const fn bypasses_rearm(self) -> bool {
        matches!(self, Self::Immediate | Self::NoEstablished { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlertLevel {
    Warning,
    Critical,
}

impl AlertLevel {
    const fn severity(self) -> NotificationSeverity {
        match self {
            Self::Warning => NotificationSeverity::Warning,
            Self::Critical => NotificationSeverity::Critical,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Warning => "WARN",
            Self::Critical => "CRITICAL",
        }
    }
}

const fn level_rank(level: AlertLevel) -> u8 {
    match level {
        AlertLevel::Warning => 1,
        AlertLevel::Critical => 2,
    }
}

#[derive(Debug, Clone, Copy)]
enum StateAction {
    Alert { level: AlertLevel, repeated: bool },
    Recovery { opened_at_unix_ms: u64 },
}

fn target_condition(
    target: &TargetObservation,
    no_established_alert_samples: u32,
) -> AlertCondition {
    if target.status == HealthStatus::Unknown {
        return AlertCondition::Unknown;
    }
    let has_network_reason = target.reasons.iter().any(|reason| {
        reason == "target process is missing"
            || reason.ends_with("matching processes found")
            || reason.starts_with("RX has remained zero")
            || reason == "no established TCP socket"
            || reason.starts_with("receive queue")
            || reason.starts_with("TCP retransmissions")
    });
    if !has_network_reason {
        return AlertCondition::Healthy;
    }
    let has_immediate_reason = target.reasons.iter().any(|reason| {
        reason == "target process is missing"
            || reason.ends_with("matching processes found")
            || reason.starts_with("RX has remained zero")
    });
    let has_no_established = target
        .reasons
        .iter()
        .any(|reason| reason == "no established TCP socket");
    let gate = if has_immediate_reason || (has_no_established && no_established_alert_samples == 1)
    {
        AlertGate::Immediate
    } else if has_no_established {
        AlertGate::NoEstablished {
            required_samples: no_established_alert_samples,
        }
    } else {
        AlertGate::Default
    };
    match target.status {
        HealthStatus::Critical => AlertCondition::Alert {
            level: AlertLevel::Critical,
            gate,
        },
        HealthStatus::Warn => AlertCondition::Alert {
            level: AlertLevel::Warning,
            gate,
        },
        HealthStatus::Ok => AlertCondition::Healthy,
        HealthStatus::Unknown => AlertCondition::Unknown,
    }
}

const fn system_condition(system: &SystemObservation) -> AlertCondition {
    match system.status {
        HealthStatus::Ok => AlertCondition::Healthy,
        HealthStatus::Warn => AlertCondition::Alert {
            level: AlertLevel::Warning,
            gate: AlertGate::Default,
        },
        HealthStatus::Critical => AlertCondition::Alert {
            level: AlertLevel::Critical,
            gate: AlertGate::Default,
        },
        HealthStatus::Unknown => AlertCondition::Unknown,
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
enum NotificationSeverity {
    Info,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Serialize)]
struct NotifyRequest {
    source: String,
    title: String,
    message: String,
    severity: NotificationSeverity,
    fields: BTreeMap<String, String>,
    dedup_key: Option<String>,
}

fn target_state_request(
    snapshot: &Snapshot,
    target: &TargetObservation,
    action: StateAction,
) -> NotifyRequest {
    let mut fields = target_fields(snapshot, target);
    let (title, message, severity) = match action {
        StateAction::Alert { level, repeated } => {
            fields.insert("state".to_owned(), level.label().to_owned());
            let qualifier = if repeated { " ongoing" } else { "" };
            (
                format!(
                    "Market data network {}{}: {}",
                    level.label(),
                    qualifier,
                    target.name
                ),
                network_evidence(target),
                level.severity(),
            )
        }
        StateAction::Recovery { opened_at_unix_ms } => {
            let duration_secs =
                snapshot.timestamp_unix_ms.saturating_sub(opened_at_unix_ms) / 1_000;
            fields.insert("state".to_owned(), "RECOVERED".to_owned());
            fields.insert("incident_duration_s".to_owned(), duration_secs.to_string());
            (
                format!("Market data network recovered: {}", target.name),
                "Market data flow returned below all notification thresholds.".to_owned(),
                NotificationSeverity::Info,
            )
        }
    };
    NotifyRequest {
        source: "public-infra-monitor".to_owned(),
        title,
        message,
        severity,
        fields,
        dedup_key: Some(format!("public-infra-monitor:target:{}:state", target.name)),
    }
}

fn system_state_request(snapshot: &Snapshot, action: StateAction) -> NotifyRequest {
    let mut fields = BTreeMap::from([
        ("host".to_owned(), snapshot.host.hostname.clone()),
        ("interface".to_owned(), snapshot.system.interface.clone()),
        ("evidence".to_owned(), snapshot.system.reasons.join("; ")),
    ]);
    let (title, message, severity) = match action {
        StateAction::Alert { level, repeated } => {
            fields.insert("state".to_owned(), level.label().to_owned());
            let qualifier = if repeated { " ongoing" } else { "" };
            (
                format!("Host network {}{}", level.label(), qualifier),
                snapshot.system.reasons.join("; "),
                level.severity(),
            )
        }
        StateAction::Recovery { opened_at_unix_ms } => {
            let duration_secs =
                snapshot.timestamp_unix_ms.saturating_sub(opened_at_unix_ms) / 1_000;
            fields.insert("state".to_owned(), "RECOVERED".to_owned());
            fields.insert("incident_duration_s".to_owned(), duration_secs.to_string());
            (
                "Host network recovered".to_owned(),
                "NIC and softnet counters returned below alert thresholds.".to_owned(),
                NotificationSeverity::Info,
            )
        }
    };
    NotifyRequest {
        source: "public-infra-monitor".to_owned(),
        title,
        message,
        severity,
        fields,
        dedup_key: Some("public-infra-monitor:system:state".to_owned()),
    }
}

fn target_fields(snapshot: &Snapshot, target: &TargetObservation) -> BTreeMap<String, String> {
    let network = &target.network;
    let mut fields = BTreeMap::from([
        ("host".to_owned(), snapshot.host.hostname.clone()),
        ("target".to_owned(), target.name.clone()),
        ("venue".to_owned(), target.venue.clone()),
        (
            "pid".to_owned(),
            target
                .process
                .as_ref()
                .map(|process| process.pid.to_string())
                .unwrap_or_else(|| "missing".to_owned()),
        ),
        (
            "connections".to_owned(),
            format!(
                "{}/{} established",
                network.established_count, network.socket_count
            ),
        ),
        (
            "recv_queue_bytes".to_owned(),
            network.recv_queue_bytes.to_string(),
        ),
    ]);
    insert_optional(&mut fields, "rx_bytes", network.rx_bytes);
    insert_optional(&mut fields, "reconnects", network.reconnects);
    insert_optional(&mut fields, "disconnects", network.disconnects);
    insert_optional(&mut fields, "retransmits", network.retransmits);
    insert_optional(&mut fields, "socket_drops", network.socket_drops);
    if let Some(idle_secs) = network.rx_idle_secs {
        fields.insert("rx_idle_s".to_owned(), format!("{idle_secs:.1}"));
    }
    if let Some(event) = target
        .bpf_events
        .iter()
        .max_by_key(|event| event.last_kernel_monotonic_ns)
    {
        fields.insert(
            "last_flow".to_owned(),
            format!(
                "{}:{} -> {}:{} ({}, count={})",
                event.local.address,
                event.local.port,
                event.remote.address,
                event.remote.port,
                event.kind,
                event.count
            ),
        );
    }
    fields
}

fn insert_optional(fields: &mut BTreeMap<String, String>, key: &str, value: Option<u64>) {
    if let Some(value) = value {
        fields.insert(key.to_owned(), value.to_string());
    }
}

fn network_evidence(target: &TargetObservation) -> String {
    let evidence = target
        .reasons
        .iter()
        .filter(|reason| reason.as_str() != "no completed sampling window")
        .cloned()
        .collect::<Vec<_>>()
        .join("; ");
    if evidence.is_empty() {
        "Network alert threshold exceeded.".to_owned()
    } else {
        evidence
    }
}

struct NotificationClient {
    address: SocketAddr,
    timeout: Duration,
    api_token: Option<String>,
}

impl NotificationClient {
    fn send(&self, request: &NotifyRequest) -> Result<()> {
        let body = serde_json::to_vec(request).context("serialize notification request")?;
        let mut stream = TcpStream::connect_timeout(&self.address, self.timeout)
            .with_context(|| format!("connect notification server {}", self.address))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .context("set notification read timeout")?;
        stream
            .set_write_timeout(Some(self.timeout))
            .context("set notification write timeout")?;
        write!(
            stream,
            "POST {NOTIFY_PATH} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.address,
            body.len()
        )
        .context("write notification request headers")?;
        if let Some(token) = self.api_token.as_deref() {
            write!(stream, "Authorization: Bearer {token}\r\n")
                .context("write notification authorization header")?;
        }
        stream
            .write_all(b"\r\n")
            .context("finish notification request headers")?;
        stream
            .write_all(&body)
            .context("write notification request body")?;
        stream.flush().context("flush notification request")?;

        let mut status_line = String::new();
        BufReader::new(stream)
            .read_line(&mut status_line)
            .context("read notification response status")?;
        let status = status_line
            .split_ascii_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u16>().ok())
            .with_context(|| format!("invalid notification response: {}", status_line.trim()))?;
        if status != 202 {
            bail!("notification server returned HTTP {status}");
        }
        Ok(())
    }
}

fn delivery_worker(
    receiver: Receiver<NotifyRequest>,
    client: NotificationClient,
    stats: Arc<NotificationStats>,
) {
    while let Ok(request) = receiver.recv() {
        let mut last_error = None;
        for attempt in 1..=DELIVERY_ATTEMPTS {
            match client.send(&request) {
                Ok(()) => {
                    stats.accepted.fetch_add(1, Ordering::Relaxed);
                    last_error = None;
                    break;
                }
                Err(error) => {
                    last_error = Some(error);
                    if attempt < DELIVERY_ATTEMPTS {
                        thread::sleep(Duration::from_millis(100 * u64::from(attempt)));
                    }
                }
            }
        }
        if let Some(error) = last_error {
            stats.failed.fetch_add(1, Ordering::Relaxed);
            warn!(
                error = %error,
                dedup_key = request.dedup_key.as_deref().unwrap_or("none"),
                "local notification delivery failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Read, net::TcpListener, sync::mpsc::TryRecvError};

    use super::*;
    use crate::model::{
        Capabilities, HostObservation, NetworkWindow, ProcessObservation, SoftnetObservation,
    };

    fn process() -> ProcessObservation {
        ProcessObservation {
            pid: 42,
            executable: "spread_pbs".to_owned(),
            cmdline: "spread_pbs --venue test".to_owned(),
            cwd: None,
            affinity: vec![8],
            current_cpu: Some(8),
            uptime_secs: Some(100.0),
            cpu_percent: Some(1.0),
        }
    }

    fn snapshot(
        timestamp_unix_ms: u64,
        status: HealthStatus,
        reasons: Vec<String>,
        disconnects: u64,
    ) -> Snapshot {
        Snapshot {
            timestamp_unix_ms,
            window_secs: Some(10.0),
            host: HostObservation {
                hostname: "test-host".to_owned(),
                kernel_release: "test".to_owned(),
            },
            targets: vec![TargetObservation {
                name: "spp_test".to_owned(),
                venue: "test-venue".to_owned(),
                expected_cpu: 8,
                process: Some(process()),
                process_candidates: 1,
                sockets: Vec::new(),
                network: NetworkWindow {
                    socket_count: 2,
                    established_count: 2,
                    rx_bytes: Some(100),
                    rx_idle_secs: Some(0.0),
                    reconnects: Some(0),
                    disconnects: Some(disconnects),
                    retransmits: Some(0),
                    socket_drops: Some(0),
                    ..NetworkWindow::default()
                },
                bpf_events: Vec::new(),
                status,
                reasons,
            }],
            system: SystemObservation {
                interface: "eth0".to_owned(),
                status: HealthStatus::Ok,
                reasons: vec!["NIC and softnet window counters are healthy".to_owned()],
                nic: BTreeMap::new(),
                tcp: BTreeMap::new(),
                softnet: SoftnetObservation::default(),
            },
            capabilities: Capabilities::default(),
        }
    }

    fn test_manager() -> (
        NotificationManager,
        Receiver<NotifyRequest>,
        Arc<NotificationStats>,
    ) {
        test_manager_with_no_established_alert_samples(1)
    }

    fn test_manager_with_no_established_alert_samples(
        no_established_alert_samples: u32,
    ) -> (
        NotificationManager,
        Receiver<NotifyRequest>,
        Arc<NotificationStats>,
    ) {
        let config = NotificationConfig {
            queue_capacity: 16,
            repeat_interval_secs: 60,
            alert_samples: 2,
            recovery_samples: 2,
            ..NotificationConfig::default()
        };
        NotificationManager::test_instance(config, no_established_alert_samples)
    }

    #[test]
    fn debounces_repeats_recovery_and_rearm() {
        let (mut manager, receiver, _) = test_manager();
        manager.observe(&snapshot(
            1_000,
            HealthStatus::Warn,
            vec!["TCP retransmissions: 2".to_owned()],
            0,
        ));
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);

        manager.observe(&snapshot(
            2_000,
            HealthStatus::Warn,
            vec!["TCP retransmissions: 2".to_owned()],
            0,
        ));
        let first = receiver.try_recv().unwrap();
        assert!(first.title.contains("WARN"));

        manager.observe(&snapshot(
            20_000,
            HealthStatus::Warn,
            vec!["TCP retransmissions: 2".to_owned()],
            0,
        ));
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);

        manager.observe(&snapshot(
            62_000,
            HealthStatus::Warn,
            vec!["TCP retransmissions: 2".to_owned()],
            0,
        ));
        let repeated = receiver.try_recv().unwrap();
        assert!(repeated.title.contains("ongoing"));

        manager.observe(&snapshot(
            70_000,
            HealthStatus::Ok,
            vec!["process, affinity and market data flow are healthy".to_owned()],
            0,
        ));
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);
        manager.observe(&snapshot(
            80_000,
            HealthStatus::Ok,
            vec!["process, affinity and market data flow are healthy".to_owned()],
            0,
        ));
        let recovery = receiver.try_recv().unwrap();
        assert!(recovery.title.contains("recovered"));
        assert!(matches!(recovery.severity, NotificationSeverity::Info));

        for timestamp in [90_000, 100_000] {
            manager.observe(&snapshot(
                timestamp,
                HealthStatus::Warn,
                vec!["TCP retransmissions: 2".to_owned()],
                0,
            ));
        }
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);

        manager.observe(&snapshot(
            141_000,
            HealthStatus::Warn,
            vec!["TCP retransmissions: 2".to_owned()],
            0,
        ));
        assert!(receiver.try_recv().unwrap().title.contains("WARN"));
    }

    #[test]
    fn isolated_disconnect_is_timeline_only() {
        let (mut manager, receiver, _) = test_manager();
        manager.observe(&snapshot(
            1_000,
            HealthStatus::Ok,
            vec!["process, affinity and market data flow are healthy".to_owned()],
            1,
        ));
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);
    }

    #[test]
    fn active_flow_reconnects_and_socket_drops_are_timeline_only() {
        let (mut manager, receiver, _) = test_manager();

        for (timestamp, status, reasons) in [
            (
                1_000,
                HealthStatus::Warn,
                vec!["socket drops: 3".to_owned()],
            ),
            (
                2_000,
                HealthStatus::Critical,
                vec!["reconnects: 18".to_owned(), "socket drops: 42".to_owned()],
            ),
            (
                3_000,
                HealthStatus::Critical,
                vec!["socket drops: 16".to_owned()],
            ),
        ] {
            manager.observe(&snapshot(timestamp, status, reasons, 0));
        }

        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);
    }

    #[test]
    fn hard_socket_failure_is_immediate() {
        let (mut manager, receiver, _) = test_manager();
        manager.observe(&snapshot(
            1_000,
            HealthStatus::Critical,
            vec!["no established TCP socket".to_owned()],
            0,
        ));
        assert!(receiver.try_recv().unwrap().title.contains("CRITICAL"));
    }

    #[test]
    fn socket_failure_can_require_consecutive_samples() {
        let (mut manager, receiver, _) = test_manager_with_no_established_alert_samples(3);

        manager.observe(&snapshot(
            1_000,
            HealthStatus::Warn,
            vec!["TCP retransmissions: 2".to_owned()],
            0,
        ));
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);

        for timestamp in [2_000, 3_000] {
            manager.observe(&snapshot(
                timestamp,
                HealthStatus::Critical,
                vec!["no established TCP socket".to_owned()],
                0,
            ));
            assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);
        }

        manager.observe(&snapshot(
            4_000,
            HealthStatus::Critical,
            vec!["no established TCP socket".to_owned()],
            0,
        ));
        assert!(receiver.try_recv().unwrap().title.contains("CRITICAL"));
    }

    #[test]
    fn socket_failure_grace_applies_before_escalating_an_active_warning() {
        let (mut manager, receiver, _) = test_manager_with_no_established_alert_samples(3);

        for timestamp in [1_000, 2_000] {
            manager.observe(&snapshot(
                timestamp,
                HealthStatus::Warn,
                vec!["TCP retransmissions: 2".to_owned()],
                0,
            ));
        }
        assert!(receiver.try_recv().unwrap().title.contains("WARN"));

        for timestamp in [3_000, 4_000] {
            manager.observe(&snapshot(
                timestamp,
                HealthStatus::Critical,
                vec!["no established TCP socket".to_owned()],
                0,
            ));
            assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);
        }

        manager.observe(&snapshot(
            5_000,
            HealthStatus::Critical,
            vec!["no established TCP socket".to_owned()],
            0,
        ));
        assert!(receiver.try_recv().unwrap().title.contains("CRITICAL"));
    }

    #[test]
    fn process_failure_remains_immediate_with_socket_grace() {
        let (mut manager, receiver, _) = test_manager_with_no_established_alert_samples(3);
        manager.observe(&snapshot(
            1_000,
            HealthStatus::Critical,
            vec![
                "target process is missing".to_owned(),
                "no established TCP socket".to_owned(),
            ],
            0,
        ));

        assert!(receiver.try_recv().unwrap().title.contains("CRITICAL"));
    }

    #[test]
    fn cpu_affinity_warning_does_not_trigger_network_alert() {
        let (mut manager, receiver, _) = test_manager();
        manager.observe(&snapshot(
            1_000,
            HealthStatus::Warn,
            vec!["CPU affinity [9] differs from expected CPU 8".to_owned()],
            0,
        ));
        assert_eq!(receiver.try_recv().unwrap_err(), TryRecvError::Empty);
    }

    #[test]
    fn client_posts_existing_notification_protocol() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            while !request.windows(4).any(|value| value == b"\r\n\r\n")
                || !request
                    .windows(15)
                    .any(|value| value == b"\"source\":\"test\"")
            {
                let read = stream.read(&mut buffer).unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }
            let text = String::from_utf8_lossy(&request);
            assert!(text.starts_with("POST /v1/notify HTTP/1.1\r\n"));
            assert!(text.contains("Authorization: Bearer secret\r\n"));
            stream
                .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        let client = NotificationClient {
            address,
            timeout: Duration::from_secs(5),
            api_token: Some("secret".to_owned()),
        };
        client
            .send(&NotifyRequest {
                source: "test".to_owned(),
                title: "test".to_owned(),
                message: "test".to_owned(),
                severity: NotificationSeverity::Info,
                fields: BTreeMap::new(),
                dedup_key: None,
            })
            .unwrap();
        server.join().unwrap();
    }
}
