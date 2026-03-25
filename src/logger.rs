use crate::types::{DirectionalSignal, Evaluation, MarketSnapshot, MockTrade, UnderlyingTick};
use std::io::Write;
use std::path::Path;
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Append a serde_json::Value as a single JSONL line.
fn append_jsonl(path: &Path, value: &impl serde::Serialize) {
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            warn!("Failed to open log file {}: {}", path.display(), e);
            return;
        }
    };
    if let Ok(line) = serde_json::to_string(value) {
        let _ = writeln!(file, "{}", line);
    }
}

fn append_csv_line(path: &Path, line: &str) {
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) => {
            warn!("Failed to open CSV file {}: {}", path.display(), e);
            return;
        }
    };
    let _ = writeln!(file, "{}", line);
}

pub async fn log_binance_ticks(
    log_dir: String,
    mut rx: broadcast::Receiver<UnderlyingTick>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let path = Path::new(&log_dir).join("binance_ticks.jsonl");
    info!("Logging Binance ticks to {}", path.display());

    loop {
        tokio::select! {
            tick = rx.recv() => {
                match tick {
                    Ok(t) => append_jsonl(&path, &t),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Binance tick logger lagged {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            _ = cancel.cancelled() => return,
        }
    }
}

pub async fn log_polymarket_snapshots(
    log_dir: String,
    mut rx: broadcast::Receiver<MarketSnapshot>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let path = Path::new(&log_dir).join("polymarket_snapshots.jsonl");
    info!("Logging Polymarket snapshots to {}", path.display());

    loop {
        tokio::select! {
            snap = rx.recv() => {
                match snap {
                    Ok(s) => append_jsonl(&path, &s),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Polymarket snapshot logger lagged {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            _ = cancel.cancelled() => return,
        }
    }
}

pub async fn log_mock_trades(
    log_dir: String,
    mut rx: broadcast::Receiver<MockTrade>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let path = Path::new(&log_dir).join("mock_trades.jsonl");
    info!("Logging mock trades to {}", path.display());

    loop {
        tokio::select! {
            trade = rx.recv() => {
                match trade {
                    Ok(t) => append_jsonl(&path, &t),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Mock trade logger lagged {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            _ = cancel.cancelled() => return,
        }
    }
}

pub async fn log_signals(
    log_dir: String,
    mut rx: broadcast::Receiver<DirectionalSignal>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let path = Path::new(&log_dir).join("signals.jsonl");
    info!("Logging signals to {}", path.display());

    loop {
        tokio::select! {
            sig = rx.recv() => {
                match sig {
                    Ok(s) => append_jsonl(&path, &s),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Signal logger lagged {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            _ = cancel.cancelled() => return,
        }
    }
}

pub async fn log_evaluations(
    log_dir: String,
    mut rx: broadcast::Receiver<Evaluation>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let eval_path = Path::new(&log_dir).join("evaluations.jsonl");
    let csv_path = Path::new(&log_dir).join("latency_metrics.csv");

    // Write CSV header if file doesn't exist
    if !csv_path.exists() {
        append_csv_line(
            &csv_path,
            "timestamp,binance_to_decision_us,poly_to_decision_us,decision_duration_us",
        );
    }

    info!("Logging evaluations to {}", eval_path.display());

    loop {
        tokio::select! {
            eval = rx.recv() => {
                match eval {
                    Ok(e) => {
                        append_jsonl(&eval_path, &e);
                        let poly_us = e.poly_to_decision_us.map_or("".to_string(), |v| v.to_string());
                        let csv_line = format!(
                            "{},{},{},{}",
                            e.timestamp,
                            e.binance_to_decision_us,
                            poly_us,
                            e.decision_duration_us,
                        );
                        append_csv_line(&csv_path, &csv_line);
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("Evaluation logger lagged {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
            _ = cancel.cancelled() => return,
        }
    }
}
