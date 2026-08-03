use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

use crate::{config::TargetConfig, model::ProcessObservation};

#[derive(Debug, Clone)]
pub struct ProcessSample {
    pub observation: ProcessObservation,
    pub cpu_ticks: u64,
}

pub fn discover(
    executable: &str,
    targets: &[TargetConfig],
) -> Result<HashMap<String, Vec<ProcessSample>>> {
    let venues: HashSet<&str> = targets.iter().map(|target| target.venue.as_str()).collect();
    let mut found: HashMap<String, Vec<ProcessSample>> = targets
        .iter()
        .map(|target| (target.name.clone(), Vec::new()))
        .collect();
    let proc_uptime = read_proc_uptime().ok();

    for entry in fs::read_dir("/proc").context("read /proc")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let root = entry.path();
        let Some(args) = read_cmdline(&root.join("cmdline")) else {
            continue;
        };
        if args.first().and_then(|arg| Path::new(arg).file_name()) != Some(OsStr::new(executable)) {
            continue;
        }
        let Some(venue) = option_value(&args, "--venue") else {
            continue;
        };
        if !venues.contains(venue) {
            continue;
        }
        let Some(target) = targets.iter().find(|target| target.venue == venue) else {
            continue;
        };
        if let Ok(sample) = sample_process(pid, &root, &args, proc_uptime) {
            found.entry(target.name.clone()).or_default().push(sample);
        }
    }

    Ok(found)
}

pub fn socket_inodes(pid: u32) -> Result<HashSet<u64>> {
    let mut inodes = HashSet::new();
    let fd_dir = PathBuf::from(format!("/proc/{pid}/fd"));
    for entry in fs::read_dir(&fd_dir).with_context(|| format!("read {}", fd_dir.display()))? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let target = match fs::read_link(entry.path()) {
            Ok(target) => target,
            Err(_) => continue,
        };
        let text = target.to_string_lossy();
        if let Some(inode) = text
            .strip_prefix("socket:[")
            .and_then(|value| value.strip_suffix(']'))
            .and_then(|value| value.parse::<u64>().ok())
        {
            inodes.insert(inode);
        }
    }
    Ok(inodes)
}

fn sample_process(
    pid: u32,
    root: &Path,
    args: &[String],
    proc_uptime: Option<f64>,
) -> Result<ProcessSample> {
    let status = fs::read_to_string(root.join("status"))?;
    let affinity_text = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .map(str::trim)
        .unwrap_or_default();
    let affinity = parse_cpu_list(affinity_text)?;
    let stat = fs::read_to_string(root.join("stat"))?;
    let parsed_stat = parse_stat(&stat)?;
    let ticks_per_second = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    let uptime_secs = match (proc_uptime, ticks_per_second > 0) {
        (Some(uptime), true) => {
            Some((uptime - parsed_stat.start_ticks as f64 / ticks_per_second as f64).max(0.0))
        }
        _ => None,
    };
    let executable = fs::read_link(root.join("exe"))
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| args[0].clone());
    let cwd = fs::read_link(root.join("cwd"))
        .ok()
        .map(|path| path.display().to_string());

    Ok(ProcessSample {
        observation: ProcessObservation {
            pid,
            executable,
            cmdline: args.join(" "),
            cwd,
            affinity,
            current_cpu: Some(parsed_stat.processor),
            uptime_secs,
            cpu_percent: None,
        },
        cpu_ticks: parsed_stat.cpu_ticks,
    })
}

fn read_cmdline(path: &Path) -> Option<Vec<String>> {
    let bytes = fs::read(path).ok()?;
    let args: Vec<String> = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    (!args.is_empty()).then_some(args)
}

fn option_value<'a>(args: &'a [String], option: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|pair| pair[0] == option)
        .map(|pair| pair[1].as_str())
}

fn read_proc_uptime() -> Result<f64> {
    fs::read_to_string("/proc/uptime")?
        .split_whitespace()
        .next()
        .context("missing uptime value")?
        .parse()
        .context("parse uptime")
}

struct ParsedStat {
    cpu_ticks: u64,
    start_ticks: u64,
    processor: u32,
}

fn parse_stat(stat: &str) -> Result<ParsedStat> {
    let close = stat.rfind(')').context("invalid /proc PID stat comm")?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    // fields[0] is field 3 (state) in proc_pid_stat(5).
    let parse = |index: usize, name: &str| -> Result<u64> {
        fields
            .get(index)
            .with_context(|| format!("missing {name} in /proc PID stat"))?
            .parse()
            .with_context(|| format!("parse {name} in /proc PID stat"))
    };
    Ok(ParsedStat {
        cpu_ticks: parse(11, "utime")? + parse(12, "stime")?,
        start_ticks: parse(19, "starttime")?,
        processor: parse(36, "processor")? as u32,
    })
}

pub fn parse_cpu_list(value: &str) -> Result<Vec<u32>> {
    let mut cpus = Vec::new();
    for part in value.split(',').filter(|part| !part.is_empty()) {
        if let Some((start, end)) = part.split_once('-') {
            let start: u32 = start.parse()?;
            let end: u32 = end.parse()?;
            if end < start {
                anyhow::bail!("invalid CPU range {part}");
            }
            cpus.extend(start..=end);
        } else {
            cpus.push(part.parse()?);
        }
    }
    Ok(cpus)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_ranges() {
        assert_eq!(parse_cpu_list("1,3-5,8").unwrap(), vec![1, 3, 4, 5, 8]);
    }

    #[test]
    fn extracts_exact_option() {
        let args = vec![
            "spread_pbs".to_owned(),
            "--venue".to_owned(),
            "gate-both".to_owned(),
        ];
        assert_eq!(option_value(&args, "--venue"), Some("gate-both"));
    }
}
