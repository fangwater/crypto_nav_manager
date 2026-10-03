use anyhow::{Context, Result};
use std::{
    env, fs,
    path::Path,
    time::{Duration, SystemTime},
};

const DEFAULT_STALE_SECS: u64 = 2 * 60 * 60;
const LEGACY_STALE_SECS: u64 = 24 * 60 * 60;
const STALE_SECS_ENV: &str = "CRYPTO_NAV_RECONCILE_STALE_SECS";

pub fn cleanup_default_stale_work_dirs() -> Result<usize> {
    let stale_secs = env::var(STALE_SECS_ENV)
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()
        .with_context(|| format!("parse {STALE_SECS_ENV}"))?
        .unwrap_or(DEFAULT_STALE_SECS);
    cleanup_stale_work_dirs(
        &env::temp_dir().join("crypto_nav_rocksdb_reconcile"),
        Path::new("/proc"),
        SystemTime::now(),
        Duration::from_secs(stale_secs),
    )
}

fn cleanup_stale_work_dirs(
    root: &Path,
    proc_root: &Path,
    now: SystemTime,
    stale_age: Duration,
) -> Result<usize> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Ok(0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", root.display())),
    }
    let mut removed = 0;
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let pid = name.to_str().and_then(parse_run_pid);
        let threshold = if pid.is_some() {
            stale_age
        } else {
            stale_age.max(Duration::from_secs(LEGACY_STALE_SECS))
        };
        let newest = match newest_modified(&path) {
            Ok(modified) => modified,
            Err(error) => {
                eprintln!(
                    "skip unreadable reconcile work directory {}: {error:#}",
                    path.display()
                );
                continue;
            }
        };
        if now.duration_since(newest).unwrap_or_default() < threshold {
            continue;
        }
        if pid.is_some_and(|pid| process_owns_run(proc_root, pid)) {
            continue;
        }
        fs::remove_dir_all(&path)
            .with_context(|| format!("remove stale work directory {}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

fn parse_run_pid(name: &str) -> Option<u32> {
    let mut parts = name.split('-');
    let stamp = parts.next()?;
    let stamp = stamp.as_bytes();
    let pid = parts.next()?;
    let micros = parts.next()?;
    if parts.next().is_some()
        || stamp.len() != 16
        || !stamp[..8].iter().all(u8::is_ascii_digit)
        || stamp[8] != b'T'
        || !stamp[9..15].iter().all(u8::is_ascii_digit)
        || stamp[15] != b'Z'
        || micros.is_empty()
        || !micros.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    pid.parse::<u32>().ok().filter(|pid| *pid > 0)
}

fn process_owns_run(proc_root: &Path, pid: u32) -> bool {
    let proc_dir = proc_root.join(pid.to_string());
    match fs::read(proc_dir.join("cmdline")) {
        Ok(cmdline) => cmdline.split(|byte| *byte == 0).any(|arg| {
            arg.windows(b"reconcile_rocksdb".len())
                .any(|window| window == b"reconcile_rocksdb")
        }),
        Err(_) => proc_dir.exists(),
    }
}

fn newest_modified(path: &Path) -> Result<SystemTime> {
    let metadata = fs::symlink_metadata(path)?;
    let mut newest = metadata.modified()?;
    if metadata.file_type().is_dir() {
        for entry in fs::read_dir(path)? {
            let child = entry?.path();
            newest = newest.max(newest_modified(&child)?);
        }
    }
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    fn fixture() -> (std::path::PathBuf, SystemTime) {
        let path = env::temp_dir().join(format!(
            "reconcile_cleanup_test_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        (path, SystemTime::now())
    }

    fn age(path: &Path, now: SystemTime, duration: Duration) {
        fs::File::open(path)
            .unwrap()
            .set_modified(now - duration)
            .unwrap();
    }

    #[test]
    fn active_pid_is_preserved() {
        let (fixture, now) = fixture();
        let work = fixture.join("work");
        let proc_root = fixture.join("proc");
        let run = work.join("20261003T060000Z-12345-1791007200000000");
        fs::create_dir_all(&run).unwrap();
        fs::create_dir_all(proc_root.join("12345")).unwrap();
        fs::write(proc_root.join("12345/cmdline"), b"/bin/reconcile_rocksdb\0").unwrap();
        age(&run, now, Duration::from_secs(10_000));
        assert_eq!(
            cleanup_stale_work_dirs(&work, &proc_root, now, Duration::from_secs(7_200)).unwrap(),
            0
        );
        assert!(run.exists());
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn dead_pid_is_removed_but_recent_content_is_kept() {
        let (fixture, now) = fixture();
        let work = fixture.join("work");
        let proc_root = fixture.join("proc");
        let stale = work.join("20261003T060000Z-12345-1791007200000000");
        let recent = work.join("20261003T060000Z-12346-1791007200000001");
        let touched = work.join("20261003T060000Z-12347-1791007200000002");
        for path in [&stale, &recent, &touched] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(touched.join("recent.csv"), b"data").unwrap();
        age(&stale, now, Duration::from_secs(10_000));
        age(&touched, now, Duration::from_secs(10_000));
        assert_eq!(
            cleanup_stale_work_dirs(&work, &proc_root, now, Duration::from_secs(7_200)).unwrap(),
            1
        );
        assert!(!stale.exists());
        assert!(recent.exists());
        assert!(touched.exists());
        fs::remove_dir_all(fixture).unwrap();
    }

    #[test]
    fn legacy_directories_need_a_full_day_of_inactivity() {
        let (fixture, now) = fixture();
        let work = fixture.join("work");
        let legacy = work.join("20260730T081555Z-czvjnle_");
        fs::create_dir_all(&legacy).unwrap();
        age(&legacy, now, Duration::from_secs(3 * 60 * 60));
        assert_eq!(
            cleanup_stale_work_dirs(
                &work,
                &fixture.join("proc"),
                now,
                Duration::from_secs(7_200)
            )
            .unwrap(),
            0
        );
        age(&legacy, now, Duration::from_secs(25 * 60 * 60));
        assert_eq!(
            cleanup_stale_work_dirs(
                &work,
                &fixture.join("proc"),
                now,
                Duration::from_secs(7_200)
            )
            .unwrap(),
            1
        );
        fs::remove_dir_all(fixture).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_links_are_not_followed() {
        use std::os::unix::fs::symlink;
        let (fixture, now) = fixture();
        let work = fixture.join("work");
        let external = fixture.join("external");
        fs::create_dir(&work).unwrap();
        fs::create_dir(&external).unwrap();
        fs::write(external.join("data"), b"keep").unwrap();
        symlink(
            &external,
            work.join("20261003T060000Z-12345-1791007200000000"),
        )
        .unwrap();
        assert_eq!(
            cleanup_stale_work_dirs(
                &work,
                &fixture.join("proc"),
                now,
                Duration::from_secs(7_200)
            )
            .unwrap(),
            0
        );
        assert!(external.join("data").exists());
        fs::remove_dir_all(fixture).unwrap();
    }
}
