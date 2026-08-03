use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::{Context, Result};

use crate::model::{Counter, SoftnetCpuObservation, SoftnetObservation, SystemObservation};

#[derive(Debug, Clone, Default)]
pub struct RawSystemSample {
    pub nic: BTreeMap<String, u64>,
    pub tcp: BTreeMap<String, u64>,
    pub softnet: Vec<RawSoftnetCpu>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RawSoftnetCpu {
    pub processed: u64,
    pub dropped: u64,
    pub time_squeeze: u64,
}

pub fn collect(interface: &str) -> Result<RawSystemSample> {
    Ok(RawSystemSample {
        nic: read_nic_statistics(interface)?,
        tcp: read_protocol_counters()?,
        softnet: read_softnet()?,
    })
}

pub fn observe(
    interface: &str,
    current: &RawSystemSample,
    previous: Option<&RawSystemSample>,
) -> SystemObservation {
    let nic = counters(&current.nic, previous.map(|sample| &sample.nic));
    let tcp = counters(&current.tcp, previous.map(|sample| &sample.tcp));
    let mut softnet = SoftnetObservation::default();
    for (cpu, current_cpu) in current.softnet.iter().enumerate() {
        let previous_cpu = previous.and_then(|sample| sample.softnet.get(cpu));
        let cpu_observation = SoftnetCpuObservation {
            cpu,
            processed: counter(
                current_cpu.processed,
                previous_cpu.map(|value| value.processed),
            ),
            dropped: counter(current_cpu.dropped, previous_cpu.map(|value| value.dropped)),
            time_squeeze: counter(
                current_cpu.time_squeeze,
                previous_cpu.map(|value| value.time_squeeze),
            ),
        };
        add_counter(&mut softnet.processed, &cpu_observation.processed);
        add_counter(&mut softnet.dropped, &cpu_observation.dropped);
        add_counter(&mut softnet.time_squeeze, &cpu_observation.time_squeeze);
        softnet.per_cpu.push(cpu_observation);
    }
    SystemObservation {
        interface: interface.to_owned(),
        status: crate::model::HealthStatus::Unknown,
        reasons: Vec::new(),
        nic,
        tcp,
        softnet,
    }
}

fn read_nic_statistics(interface: &str) -> Result<BTreeMap<String, u64>> {
    if interface.contains('/') || interface == "." || interface == ".." {
        anyhow::bail!("invalid interface name {interface:?}");
    }
    let directory = PathBuf::from(format!("/sys/class/net/{interface}/statistics"));
    let mut values = BTreeMap::new();
    for entry in
        fs::read_dir(&directory).with_context(|| format!("read NIC statistics for {interface}"))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let value = fs::read_to_string(entry.path())?.trim().parse()?;
        values.insert(name, value);
    }
    Ok(values)
}

fn read_protocol_counters() -> Result<BTreeMap<String, u64>> {
    let mut counters = BTreeMap::new();
    for path in ["/proc/net/snmp", "/proc/net/netstat"] {
        let text = fs::read_to_string(path).with_context(|| format!("read {path}"))?;
        parse_protocol_pairs(&text, &mut counters)?;
    }
    Ok(counters)
}

fn parse_protocol_pairs(text: &str, output: &mut BTreeMap<String, u64>) -> Result<()> {
    let mut lines = text.lines();
    while let Some(names_line) = lines.next() {
        let Some(values_line) = lines.next() else {
            anyhow::bail!("protocol counter header without values");
        };
        let (section, names) = names_line
            .split_once(':')
            .context("protocol counter header without section")?;
        let (value_section, values) = values_line
            .split_once(':')
            .context("protocol counter values without section")?;
        if section != value_section {
            anyhow::bail!("protocol counter section mismatch: {section} != {value_section}");
        }
        for (name, value) in names.split_whitespace().zip(values.split_whitespace()) {
            if let Ok(value) = value.parse::<u64>() {
                output.insert(format!("{section}.{name}"), value);
            }
        }
    }
    Ok(())
}

fn read_softnet() -> Result<Vec<RawSoftnetCpu>> {
    fs::read_to_string("/proc/net/softnet_stat")?
        .lines()
        .map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 3 {
                anyhow::bail!("short softnet_stat line");
            }
            Ok(RawSoftnetCpu {
                processed: u64::from_str_radix(fields[0], 16)?,
                dropped: u64::from_str_radix(fields[1], 16)?,
                time_squeeze: u64::from_str_radix(fields[2], 16)?,
            })
        })
        .collect()
}

fn counters(
    current: &BTreeMap<String, u64>,
    previous: Option<&BTreeMap<String, u64>>,
) -> BTreeMap<String, Counter> {
    current
        .iter()
        .map(|(name, value)| {
            let before = previous.and_then(|values| values.get(name)).copied();
            (name.clone(), counter(*value, before))
        })
        .collect()
}

fn counter(total: u64, previous: Option<u64>) -> Counter {
    Counter {
        total,
        delta: previous.and_then(|value| total.checked_sub(value)),
    }
}

fn add_counter(total: &mut Counter, value: &Counter) {
    total.total = total.total.saturating_add(value.total);
    total.delta = match (total.delta, value.delta) {
        (None, Some(delta)) if total.total == value.total => Some(delta),
        (Some(left), Some(right)) => Some(left.saturating_add(right)),
        _ => None,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_protocol_counter_pairs() {
        let mut output = BTreeMap::new();
        parse_protocol_pairs("Tcp: RetransSegs InErrs\nTcp: 2 3\n", &mut output).unwrap();
        assert_eq!(output["Tcp.RetransSegs"], 2);
        assert_eq!(output["Tcp.InErrs"], 3);
    }

    #[test]
    fn counter_reset_has_unknown_delta() {
        assert_eq!(counter(5, Some(8)).delta, None);
        assert_eq!(counter(9, Some(8)).delta, Some(1));
    }
}
