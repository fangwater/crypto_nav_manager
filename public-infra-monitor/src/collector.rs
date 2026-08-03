use std::{
    collections::{HashMap, HashSet},
    fs,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use tracing::warn;

use crate::{
    bpf::BpfMonitor,
    config::MonitorConfig,
    health,
    model::{
        Capabilities, HealthStatus, HostObservation, NetworkWindow, Snapshot, SocketObservation,
        TargetObservation,
    },
    procfs::{self, ProcessSample},
    sock_diag, system,
};

pub struct Monitor {
    config: MonitorConfig,
    previous: Option<RawSnapshot>,
    bpf: Option<BpfMonitor>,
    bpf_reason: Option<String>,
    rx_idle_secs: HashMap<String, f64>,
}

struct RawSnapshot {
    instant: Instant,
    processes: HashMap<String, Vec<ProcessSample>>,
    sockets: Vec<SocketObservation>,
    socket_inodes: HashMap<u32, HashSet<u64>>,
    system: system::RawSystemSample,
}

impl Monitor {
    pub fn new(config: MonitorConfig) -> Self {
        let (bpf, bpf_reason) = if config.bpf_enabled {
            match BpfMonitor::load() {
                Ok(monitor) => (Some(monitor), None),
                Err(error) => {
                    warn!(error = %error, "BPF unavailable; continuing with INET_DIAG");
                    (None, Some(error.to_string()))
                }
            }
        } else {
            (None, Some("disabled by configuration".to_owned()))
        };
        Self {
            config,
            previous: None,
            bpf,
            bpf_reason,
            rx_idle_secs: HashMap::new(),
        }
    }

    pub fn sample(&mut self) -> Result<Snapshot> {
        let raw = self.collect_raw()?;
        let window_secs = self
            .previous
            .as_ref()
            .map(|previous| raw.instant.duration_since(previous.instant).as_secs_f64());
        let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
        let mut targets: Vec<TargetObservation> = self
            .config
            .targets
            .iter()
            .map(|target| {
                let candidates = raw.processes.get(&target.name).cloned().unwrap_or_default();
                let mut process =
                    (candidates.len() == 1).then(|| candidates[0].observation.clone());
                if let (Some(process), Some(window_secs), Some(previous)) =
                    (process.as_mut(), window_secs, self.previous.as_ref())
                    && let Some(before) = previous
                        .processes
                        .get(&target.name)
                        .and_then(|samples| (samples.len() == 1).then(|| &samples[0]))
                        .filter(|sample| sample.observation.pid == process.pid)
                {
                    process.cpu_percent = candidates[0]
                        .cpu_ticks
                        .checked_sub(before.cpu_ticks)
                        .map(|ticks| ticks as f64 / ticks_per_second / window_secs * 100.0);
                }

                let inodes = process
                    .as_ref()
                    .and_then(|process| raw.socket_inodes.get(&process.pid));
                let sockets: Vec<SocketObservation> = raw
                    .sockets
                    .iter()
                    .filter(|socket| inodes.is_some_and(|set| set.contains(&socket.inode)))
                    .cloned()
                    .collect();
                let previous_sockets = self.previous.as_ref().and_then(|previous| {
                    let previous_candidates = previous.processes.get(&target.name)?;
                    if previous_candidates.len() != 1 {
                        return None;
                    }
                    let previous_inodes = previous
                        .socket_inodes
                        .get(&previous_candidates[0].observation.pid)?;
                    Some(
                        previous
                            .sockets
                            .iter()
                            .filter(|socket| previous_inodes.contains(&socket.inode))
                            .map(|socket| (socket.inode, socket))
                            .collect::<HashMap<_, _>>(),
                    )
                });
                let network = aggregate_network(&sockets, previous_sockets.as_ref());
                TargetObservation {
                    name: target.name.clone(),
                    venue: target.venue.clone(),
                    expected_cpu: target.expected_cpu,
                    process,
                    process_candidates: candidates.len(),
                    sockets,
                    network,
                    bpf_events: Vec::new(),
                    status: HealthStatus::Unknown,
                    reasons: Vec::new(),
                }
            })
            .collect();

        let bpf_result = self
            .bpf
            .as_mut()
            .map(|bpf| bpf.sample(&targets, window_secs.is_some()));
        let mut bpf_windows = match bpf_result {
            Some(Ok(windows)) => windows,
            Some(Err(error)) => {
                warn!(error = %error, "BPF map sampling failed; disabling BPF collector");
                self.bpf_reason = Some(error.to_string());
                self.bpf = None;
                HashMap::new()
            }
            None => HashMap::new(),
        };
        for target in &mut targets {
            if let Some(window) = bpf_windows.remove(&target.name) {
                if window_secs.is_some() {
                    target.network.retransmits = Some(
                        target
                            .network
                            .retransmits
                            .unwrap_or_default()
                            .max(window.retransmits),
                    );
                    target.network.reconnects = Some(
                        target
                            .network
                            .reconnects
                            .unwrap_or_default()
                            .max(window.establishes),
                    );
                    target.network.disconnects = Some(window.disconnects);
                }
                target.bpf_events = window.events;
            }
            let idle_secs = self.rx_idle_secs.entry(target.name.clone()).or_default();
            match (target.network.rx_bytes, window_secs) {
                (Some(0), Some(window_secs)) => *idle_secs += window_secs,
                (Some(_), Some(_)) => *idle_secs = 0.0,
                _ => {}
            }
            target.network.rx_idle_secs = window_secs.map(|_| *idle_secs);
            (target.status, target.reasons) = health::assess(
                target.process.as_ref(),
                target.process_candidates,
                target.expected_cpu,
                &target.network,
                &self.config.thresholds,
            );
        }

        let mut system = system::observe(
            &self.config.interface,
            &raw.system,
            self.previous.as_ref().map(|previous| &previous.system),
        );
        (system.status, system.reasons) = health::assess_system(&system, &self.config.thresholds);

        let snapshot = Snapshot {
            timestamp_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or(Duration::ZERO)
                .as_millis() as u64,
            window_secs,
            host: read_host(),
            targets,
            system,
            capabilities: Capabilities {
                inet_diag: true,
                nic_standard_stats: true,
                ethtool_stats: false,
                runqlat: false,
                bpf: self.bpf.is_some(),
                bpf_reason: self.bpf_reason.clone(),
            },
        };
        self.previous = Some(raw);
        Ok(snapshot)
    }

    fn collect_raw(&self) -> Result<RawSnapshot> {
        let processes = procfs::discover(&self.config.executable, &self.config.targets)?;
        let mut socket_inodes = HashMap::new();
        for candidates in processes.values() {
            for process in candidates {
                if let Ok(inodes) = procfs::socket_inodes(process.observation.pid) {
                    socket_inodes.insert(process.observation.pid, inodes);
                }
            }
        }
        Ok(RawSnapshot {
            instant: Instant::now(),
            processes,
            sockets: sock_diag::query_tcp().context("query TCP sockets through INET_DIAG")?,
            socket_inodes,
            system: system::collect(&self.config.interface)?,
        })
    }
}

fn aggregate_network(
    sockets: &[SocketObservation],
    previous: Option<&HashMap<u64, &SocketObservation>>,
) -> NetworkWindow {
    let mut network = NetworkWindow {
        socket_count: sockets.len(),
        established_count: sockets
            .iter()
            .filter(|socket| socket.state == "ESTABLISHED")
            .count(),
        rx_bytes: previous.map(|_| 0),
        tx_bytes: previous.map(|_| 0),
        retransmits: previous.map(|_| 0),
        socket_drops: previous.map(|_| 0),
        reconnects: previous.map(|_| 0),
        ..NetworkWindow::default()
    };
    for socket in sockets {
        network.recv_queue_bytes = network
            .recv_queue_bytes
            .saturating_add(socket.recv_queue_bytes);
        network.send_queue_bytes = network
            .send_queue_bytes
            .saturating_add(socket.send_queue_bytes);
        network.max_rtt_us = max_option(network.max_rtt_us, socket.rtt_us);
        network.max_rto_us = max_option(network.max_rto_us, socket.rto_us);
        network.max_last_data_recv_ms =
            max_option(network.max_last_data_recv_ms, socket.last_data_recv_ms);

        let Some(previous) = previous else { continue };
        let before = previous.get(&socket.inode).copied();
        add_delta(
            &mut network.rx_bytes,
            socket.bytes_received,
            before.and_then(|value| value.bytes_received),
        );
        add_delta(
            &mut network.tx_bytes,
            socket.bytes_sent,
            before.and_then(|value| value.bytes_sent),
        );
        add_delta_u32(
            &mut network.retransmits,
            socket.total_retrans,
            before.and_then(|value| value.total_retrans),
        );
        add_delta_u32(
            &mut network.socket_drops,
            socket.socket_drops,
            before.and_then(|value| value.socket_drops),
        );
        if before.is_none() && socket.state == "ESTABLISHED" {
            network.reconnects = network.reconnects.map(|value| value.saturating_add(1));
        }
    }
    network
}

fn add_delta(total: &mut Option<u64>, current: Option<u64>, previous: Option<u64>) {
    let Some(total) = total else { return };
    let Some(current) = current else { return };
    let delta = previous
        .and_then(|before| current.checked_sub(before))
        .unwrap_or(current);
    *total = total.saturating_add(delta);
}

fn add_delta_u32(total: &mut Option<u64>, current: Option<u32>, previous: Option<u32>) {
    add_delta(total, current.map(u64::from), previous.map(u64::from));
}

fn max_option<T: Ord>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.max(right)),
        (value @ Some(_), None) | (None, value @ Some(_)) => value,
        (None, None) => None,
    }
}

fn read_host() -> HostObservation {
    HostObservation {
        hostname: fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|_| "unknown".to_owned()),
        kernel_release: fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|value| value.trim().to_owned())
            .unwrap_or_else(|_| "unknown".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Endpoint;

    fn socket(inode: u64, rx: u64, retrans: u32) -> SocketObservation {
        SocketObservation {
            inode,
            family: "ipv4".to_owned(),
            state: "ESTABLISHED".to_owned(),
            local: Endpoint {
                address: "127.0.0.1".to_owned(),
                port: 1,
            },
            remote: Endpoint {
                address: "127.0.0.2".to_owned(),
                port: 2,
            },
            recv_queue_bytes: 0,
            send_queue_bytes: 0,
            rtt_us: Some(10),
            rto_us: Some(200_000),
            snd_cwnd: Some(10),
            last_data_recv_ms: Some(1),
            bytes_received: Some(rx),
            bytes_sent: Some(0),
            total_retrans: Some(retrans),
            socket_drops: Some(0),
        }
    }

    #[test]
    fn computes_surviving_and_new_socket_deltas() {
        let before_socket = socket(1, 100, 1);
        let previous = HashMap::from([(1, &before_socket)]);
        let current = vec![socket(1, 160, 3), socket(2, 40, 1)];
        let window = aggregate_network(&current, Some(&previous));
        assert_eq!(window.rx_bytes, Some(100));
        assert_eq!(window.retransmits, Some(3));
        assert_eq!(window.reconnects, Some(1));
    }
}
