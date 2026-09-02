use crate::{
    config::Thresholds,
    model::{HealthStatus, NetworkWindow, ProcessObservation, SystemObservation},
};

pub fn assess(
    process: Option<&ProcessObservation>,
    candidates: usize,
    expected_cpu: u32,
    network: &NetworkWindow,
    thresholds: &Thresholds,
) -> (HealthStatus, Vec<String>) {
    let mut status = HealthStatus::Ok;
    let mut reasons = Vec::new();

    if candidates == 0 {
        return (
            HealthStatus::Critical,
            vec!["target process is missing".to_owned()],
        );
    }
    if candidates > 1 {
        return (
            HealthStatus::Critical,
            vec![format!("{} matching processes found", candidates)],
        );
    }

    let Some(process) = process else {
        return (
            HealthStatus::Critical,
            vec!["target process is missing".to_owned()],
        );
    };
    if process.affinity != [expected_cpu] {
        raise(
            &mut status,
            HealthStatus::Warn,
            &mut reasons,
            &format!(
                "CPU affinity {:?} differs from expected CPU {}",
                process.affinity, expected_cpu
            ),
        );
    }
    if process
        .current_cpu
        .is_some_and(|cpu| !process.affinity.contains(&cpu))
    {
        raise(
            &mut status,
            HealthStatus::Warn,
            &mut reasons,
            "current CPU is outside configured affinity",
        );
    }

    if network.rx_bytes.is_none() {
        raise(
            &mut status,
            HealthStatus::Unknown,
            &mut reasons,
            "no completed sampling window",
        );
    }

    if let Some(idle_secs) = network.rx_idle_secs {
        if idle_secs >= thresholds.rx_idle_critical_secs as f64 {
            raise(
                &mut status,
                HealthStatus::Critical,
                &mut reasons,
                &format!("RX has remained zero for {idle_secs:.1}s"),
            );
        } else if idle_secs >= thresholds.rx_idle_warn_secs as f64 {
            raise(
                &mut status,
                HealthStatus::Warn,
                &mut reasons,
                &format!("RX has remained zero for {idle_secs:.1}s"),
            );
        }
    }

    if network.socket_count == 0 || network.established_count == 0 {
        raise(
            &mut status,
            HealthStatus::Critical,
            &mut reasons,
            "no established TCP socket",
        );
    }
    if network.recv_queue_bytes >= thresholds.recv_queue_critical_bytes {
        raise(
            &mut status,
            HealthStatus::Critical,
            &mut reasons,
            "receive queue exceeds critical threshold",
        );
    } else if network.recv_queue_bytes >= thresholds.recv_queue_warn_bytes {
        raise(
            &mut status,
            HealthStatus::Warn,
            &mut reasons,
            "receive queue exceeds warning threshold",
        );
    }
    check_counter(
        network.retransmits,
        thresholds.retrans_warn,
        thresholds.retrans_critical,
        "TCP retransmissions",
        &mut status,
        &mut reasons,
    );
    check_counter(
        network.socket_drops,
        thresholds.socket_drop_warn,
        thresholds.socket_drop_critical,
        "socket drops",
        &mut status,
        &mut reasons,
    );
    check_counter(
        network.reconnects,
        thresholds.reconnect_warn,
        thresholds.reconnect_critical,
        "reconnects",
        &mut status,
        &mut reasons,
    );

    if reasons.is_empty() {
        reasons.push("process, affinity, sockets and window counters are healthy".to_owned());
    }
    (status, reasons)
}

pub fn assess_system(
    system: &SystemObservation,
    thresholds: &Thresholds,
) -> (HealthStatus, Vec<String>) {
    if system.softnet.dropped.delta.is_none() {
        return (
            HealthStatus::Unknown,
            vec!["no completed system sampling window".to_owned()],
        );
    }

    let mut status = HealthStatus::Ok;
    let mut reasons = Vec::new();
    let nic_drops: u64 = [
        "rx_errors",
        "rx_dropped",
        "rx_missed_errors",
        "tx_errors",
        "tx_dropped",
    ]
    .iter()
    .filter_map(|name| system.nic.get(*name).and_then(|counter| counter.delta))
    .sum();
    if nic_drops >= thresholds.socket_drop_critical {
        raise(
            &mut status,
            HealthStatus::Critical,
            &mut reasons,
            &format!("NIC error/drop delta: {nic_drops}"),
        );
    } else if nic_drops >= thresholds.socket_drop_warn {
        raise(
            &mut status,
            HealthStatus::Warn,
            &mut reasons,
            &format!("NIC error/drop delta: {nic_drops}"),
        );
    }

    check_counter(
        system.softnet.dropped.delta,
        thresholds.softnet_drop_warn,
        thresholds.socket_drop_critical,
        "softnet drops",
        &mut status,
        &mut reasons,
    );
    check_counter(
        system.softnet.time_squeeze.delta,
        thresholds.softnet_time_squeeze_warn,
        thresholds.socket_drop_critical,
        "softnet time_squeeze",
        &mut status,
        &mut reasons,
    );
    if reasons.is_empty() {
        reasons.push("NIC and softnet window counters are healthy".to_owned());
    }
    (status, reasons)
}

fn check_counter(
    value: Option<u64>,
    warn: u64,
    critical: u64,
    label: &str,
    status: &mut HealthStatus,
    reasons: &mut Vec<String>,
) {
    let Some(value) = value else { return };
    if value >= critical {
        raise(
            status,
            HealthStatus::Critical,
            reasons,
            &format!("{label}: {value}"),
        );
    } else if value >= warn {
        raise(
            status,
            HealthStatus::Warn,
            reasons,
            &format!("{label}: {value}"),
        );
    }
}

fn raise(
    status: &mut HealthStatus,
    candidate: HealthStatus,
    reasons: &mut Vec<String>,
    reason: &str,
) {
    if severity(candidate) > severity(*status) {
        *status = candidate;
    }
    reasons.push(reason.to_owned());
}

const fn severity(status: HealthStatus) -> u8 {
    match status {
        HealthStatus::Ok => 0,
        HealthStatus::Unknown => 1,
        HealthStatus::Warn => 2,
        HealthStatus::Critical => 3,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::model::{Counter, SoftnetObservation};

    #[test]
    fn sustained_zero_rx_becomes_critical() {
        let process = ProcessObservation {
            pid: 1,
            executable: "spread_pbs".to_owned(),
            cmdline: "spread_pbs --venue test".to_owned(),
            cwd: None,
            affinity: vec![8],
            current_cpu: Some(8),
            uptime_secs: Some(10.0),
            cpu_percent: Some(1.0),
        };
        let network = NetworkWindow {
            socket_count: 1,
            established_count: 1,
            rx_bytes: Some(0),
            rx_idle_secs: Some(121.0),
            ..NetworkWindow::default()
        };
        let (status, _) = assess(Some(&process), 1, 8, &network, &Thresholds::default());
        assert_eq!(status, HealthStatus::Critical);
    }

    #[test]
    fn multiple_candidates_are_reported_as_duplicates() {
        let (status, reasons) = assess(
            None,
            2,
            8,
            &NetworkWindow::default(),
            &Thresholds::default(),
        );

        assert_eq!(status, HealthStatus::Critical);
        assert_eq!(reasons, ["2 matching processes found"]);
    }

    #[test]
    fn softnet_drop_is_system_warning() {
        let system = SystemObservation {
            interface: "eth0".to_owned(),
            status: HealthStatus::Unknown,
            reasons: Vec::new(),
            nic: BTreeMap::new(),
            tcp: BTreeMap::new(),
            softnet: SoftnetObservation {
                dropped: Counter {
                    total: 1,
                    delta: Some(1),
                },
                processed: Counter {
                    total: 10,
                    delta: Some(10),
                },
                time_squeeze: Counter {
                    total: 0,
                    delta: Some(0),
                },
                per_cpu: Vec::new(),
            },
        };
        let (status, _) = assess_system(&system, &Thresholds::default());
        assert_eq!(status, HealthStatus::Warn);
    }
}
