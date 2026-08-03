mod bpf;
mod collector;
mod config;
mod health;
mod model;
mod procfs;
mod sock_diag;
mod system;

use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use tokio::{net::TcpListener, sync::RwLock};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::{
    collector::Monitor,
    config::MonitorConfig,
    model::{HealthStatus, Snapshot},
};

type SharedSnapshot = Arc<RwLock<Option<Snapshot>>>;

#[derive(Debug, Parser)]
#[command(version, about = "Read-only process network health monitor")]
struct Args {
    #[arg(long, default_value = "config.json")]
    config: PathBuf,

    #[arg(long)]
    once: bool,

    #[arg(long)]
    no_bpf: bool,

    #[arg(long)]
    window_secs: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let bytes = fs::read(&args.config)
        .with_context(|| format!("read configuration {}", args.config.display()))?;
    let mut config: MonitorConfig = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse configuration {}", args.config.display()))?;
    config.validate()?;
    if args.no_bpf {
        config.bpf_enabled = false;
    }
    if config.bpf_enabled && !cfg!(feature = "bpf") {
        warn!("BPF requested but binary was built without the bpf feature");
    }

    if args.once {
        return run_once(config, args.window_secs).await;
    }
    run_daemon(config).await
}

async fn run_once(config: MonitorConfig, window_secs: Option<u64>) -> Result<()> {
    let interval = window_secs.unwrap_or(config.sample_interval_secs).max(1);
    let mut monitor = Monitor::new(config);
    monitor.sample().context("collect baseline sample")?;
    tokio::time::sleep(Duration::from_secs(interval)).await;
    let snapshot = monitor.sample().context("collect window sample")?;
    println!("{}", serde_json::to_string_pretty(&snapshot)?);
    Ok(())
}

async fn run_daemon(config: MonitorConfig) -> Result<()> {
    let listen = config.listen.clone();
    let interval = Duration::from_secs(config.sample_interval_secs);
    let monitor = Arc::new(Mutex::new(Monitor::new(config)));
    let initial = monitor
        .lock()
        .expect("monitor mutex poisoned")
        .sample()
        .context("collect initial sample")?;
    let snapshot = Arc::new(RwLock::new(Some(initial)));

    let collector_monitor = Arc::clone(&monitor);
    let collector_snapshot = Arc::clone(&snapshot);
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(interval);
        timer.tick().await;
        loop {
            timer.tick().await;
            let result = collector_monitor
                .lock()
                .expect("monitor mutex poisoned")
                .sample();
            match result {
                Ok(value) => *collector_snapshot.write().await = Some(value),
                Err(error) => error!(error = %error, "sampling failed"),
            }
        }
    });

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/snapshot", get(snapshot_json))
        .route("/metrics", get(metrics))
        .with_state(snapshot);
    let listener = TcpListener::bind(&listen)
        .await
        .with_context(|| format!("bind HTTP listener {listen}"))?;
    info!(listen = %listen, "public-infra-monitor listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve HTTP")?;
    Ok(())
}

async fn healthz(State(state): State<SharedSnapshot>) -> StatusCode {
    let guard = state.read().await;
    match guard.as_ref() {
        Some(snapshot)
            if snapshot.system.status != HealthStatus::Critical
                && snapshot
                    .targets
                    .iter()
                    .all(|target| target.status != HealthStatus::Critical) =>
        {
            StatusCode::OK
        }
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

async fn snapshot_json(State(state): State<SharedSnapshot>) -> Result<Json<Snapshot>, StatusCode> {
    state
        .read()
        .await
        .clone()
        .map(Json)
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

async fn metrics(State(state): State<SharedSnapshot>) -> Response {
    let guard = state.read().await;
    let Some(snapshot) = guard.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        render_metrics(snapshot),
    )
        .into_response()
}

fn render_metrics(snapshot: &Snapshot) -> String {
    let mut output = String::new();
    output.push_str("# TYPE public_infra_system_status gauge\n");
    output.push_str("# TYPE public_infra_target_status gauge\n");
    output.push_str("# TYPE public_infra_target_socket_count gauge\n");
    output.push_str("# TYPE public_infra_target_rx_bytes_window gauge\n");
    output.push_str("# TYPE public_infra_target_tx_bytes_window gauge\n");
    output.push_str("# TYPE public_infra_target_retransmits_window gauge\n");
    output.push_str("# TYPE public_infra_target_socket_drops_window gauge\n");
    output.push_str("# TYPE public_infra_target_recv_queue_bytes gauge\n");
    output.push_str("# TYPE public_infra_nic_counter_total counter\n");
    output.push_str("# TYPE public_infra_nic_counter_window gauge\n");
    output.push_str("# TYPE public_infra_tcp_counter_total counter\n");
    output.push_str("# TYPE public_infra_tcp_counter_window gauge\n");
    output.push_str("# TYPE public_infra_softnet_counter_total counter\n");
    output.push_str("# TYPE public_infra_softnet_counter_window gauge\n");
    let system_status = match snapshot.system.status {
        HealthStatus::Ok => 0,
        HealthStatus::Unknown => 1,
        HealthStatus::Warn => 2,
        HealthStatus::Critical => 3,
    };
    output.push_str(&format!("public_infra_system_status {system_status}\n"));
    for target in &snapshot.targets {
        let labels = format!(
            "name=\"{}\",venue=\"{}\"",
            prometheus_escape(&target.name),
            prometheus_escape(&target.venue)
        );
        let status = match target.status {
            HealthStatus::Ok => 0,
            HealthStatus::Unknown => 1,
            HealthStatus::Warn => 2,
            HealthStatus::Critical => 3,
        };
        output.push_str(&format!(
            "public_infra_target_status{{{labels}}} {status}\n"
        ));
        output.push_str(&format!(
            "public_infra_target_socket_count{{{labels}}} {}\n",
            target.network.socket_count
        ));
        metric_option(
            &mut output,
            "public_infra_target_rx_bytes_window",
            &labels,
            target.network.rx_bytes,
        );
        metric_option(
            &mut output,
            "public_infra_target_tx_bytes_window",
            &labels,
            target.network.tx_bytes,
        );
        metric_option(
            &mut output,
            "public_infra_target_retransmits_window",
            &labels,
            target.network.retransmits,
        );
        metric_option(
            &mut output,
            "public_infra_target_socket_drops_window",
            &labels,
            target.network.socket_drops,
        );
        output.push_str(&format!(
            "public_infra_target_recv_queue_bytes{{{labels}}} {}\n",
            target.network.recv_queue_bytes
        ));
    }

    let interface = prometheus_escape(&snapshot.system.interface);
    for (name, counter) in &snapshot.system.nic {
        let labels = format!(
            "interface=\"{interface}\",counter=\"{}\"",
            prometheus_escape(name)
        );
        counter_metrics(&mut output, "public_infra_nic_counter", &labels, counter);
    }
    for (name, counter) in &snapshot.system.tcp {
        let labels = format!("counter=\"{}\"", prometheus_escape(name));
        counter_metrics(&mut output, "public_infra_tcp_counter", &labels, counter);
    }
    for (name, counter) in [
        ("processed", &snapshot.system.softnet.processed),
        ("dropped", &snapshot.system.softnet.dropped),
        ("time_squeeze", &snapshot.system.softnet.time_squeeze),
    ] {
        let labels = format!("cpu=\"all\",counter=\"{name}\"");
        counter_metrics(
            &mut output,
            "public_infra_softnet_counter",
            &labels,
            counter,
        );
    }
    for cpu in &snapshot.system.softnet.per_cpu {
        for (name, counter) in [
            ("processed", &cpu.processed),
            ("dropped", &cpu.dropped),
            ("time_squeeze", &cpu.time_squeeze),
        ] {
            let labels = format!("cpu=\"{}\",counter=\"{name}\"", cpu.cpu);
            counter_metrics(
                &mut output,
                "public_infra_softnet_counter",
                &labels,
                counter,
            );
        }
    }
    output
}

fn counter_metrics(
    output: &mut String,
    prefix: &str,
    labels: &str,
    counter: &crate::model::Counter,
) {
    output.push_str(&format!("{prefix}_total{{{labels}}} {}\n", counter.total));
    metric_option(output, &format!("{prefix}_window"), labels, counter.delta);
}

fn metric_option(output: &mut String, name: &str, labels: &str, value: Option<u64>) {
    if let Some(value) = value {
        output.push_str(&format!("{name}{{{labels}}} {value}\n"));
    }
}

fn prometheus_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        error!(error = %error, "failed to install shutdown signal");
    }
}
