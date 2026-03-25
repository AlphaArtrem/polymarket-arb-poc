mod binance_feed;
mod config;
mod logger;
mod polymarket_feed;
mod signals;
mod state;
mod strategy;
mod types;

use crate::config::Config;
use crate::polymarket_feed::PolymarketState;
use crate::types::{DirectionalSignal, Evaluation, MarketResolution, MarketSnapshot, MockTrade, UnderlyingTick};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::info;

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Load config
    let config = Config::load(Path::new("config.toml")).expect("Failed to load config.toml");
    info!("Loaded config: {:?}", config);

    // Create log directory
    std::fs::create_dir_all(&config.general.log_dir).expect("Failed to create log directory");

    // Broadcast channels
    let (tick_tx, _) = broadcast::channel::<UnderlyingTick>(4096);
    let (snapshot_tx, _) = broadcast::channel::<MarketSnapshot>(1024);
    let (eval_tx, _) = broadcast::channel::<Evaluation>(4096);
    let (trade_tx, _) = broadcast::channel::<MockTrade>(256);
    let (signal_tx, _) = broadcast::channel::<DirectionalSignal>(256);
    let (resolution_tx, _) = broadcast::channel::<MarketResolution>(64);

    // Subscription update channel for Polymarket WS
    let (sub_tx, sub_rx) = tokio::sync::mpsc::channel(64);

    // Shared state
    let app_state = state::create_state();
    let poly_state = Arc::new(RwLock::new(PolymarketState {
        token_map: HashMap::new(),
        snapshots: HashMap::new(),
        active_markets: HashMap::new(),
    }));

    // Cancellation
    let cancel = CancellationToken::new();

    // Spawn Binance feed
    let binance_cancel = cancel.clone();
    let binance_tx = tick_tx.clone();
    let binance_symbols = config.binance.symbols.clone();
    tokio::spawn(async move {
        binance_feed::run_binance_feed(binance_symbols, binance_tx, binance_cancel).await;
    });

    // Spawn Polymarket market discovery
    let discovery_cancel = cancel.clone();
    let discovery_state = poly_state.clone();
    let discovery_symbols = config.polymarket.symbols.clone();
    tokio::spawn(async move {
        polymarket_feed::run_market_discovery(
            discovery_symbols,
            discovery_state,
            sub_tx,
            discovery_cancel,
        )
        .await;
    });

    // Spawn Polymarket CLOB WebSocket
    let clob_cancel = cancel.clone();
    let clob_state = poly_state.clone();
    let clob_snapshot_tx = snapshot_tx.clone();
    let clob_resolution_tx = resolution_tx.clone();
    tokio::spawn(async move {
        polymarket_feed::run_clob_ws(
            clob_state,
            clob_snapshot_tx,
            clob_resolution_tx,
            sub_rx,
            3, // max_levels for order book
            clob_cancel,
        )
        .await;
    });

    // Spawn strategy engine
    let strat_cancel = cancel.clone();
    let strat_tick_rx = tick_tx.subscribe();
    let strat_poly_state = poly_state.clone();
    let strat_app_state = app_state.clone();
    let strat_eval_tx = eval_tx.clone();
    let strat_trade_tx = trade_tx.clone();
    let strat_signal_tx = signal_tx.clone();
    let directional_cfg = config.directional.clone();
    let sniping_cfg = config.sniping.clone();
    let fees_cfg = config.fees.clone();
    let latency_cfg = config.latency.clone();
    tokio::spawn(async move {
        strategy::run_strategy(
            directional_cfg,
            sniping_cfg,
            fees_cfg,
            latency_cfg,
            strat_tick_rx,
            strat_poly_state,
            strat_app_state,
            strat_eval_tx,
            strat_trade_tx,
            strat_signal_tx,
            strat_cancel,
        )
        .await;
    });

    // Spawn settlement handler
    let settle_cancel = cancel.clone();
    let settle_resolution_rx = resolution_tx.subscribe();
    let settle_app_state = app_state.clone();
    tokio::spawn(async move {
        strategy::run_settlement(settle_resolution_rx, settle_app_state, settle_cancel).await;
    });

    // Spawn loggers (gated by config.logging)
    let log_dir = config.general.log_dir.clone();
    let log_cfg = &config.logging;

    if log_cfg.binance_ticks {
        let c = cancel.clone();
        let d = log_dir.clone();
        let rx = tick_tx.subscribe();
        tokio::spawn(async move { logger::log_binance_ticks(d, rx, c).await; });
    } else {
        info!("Binance tick logging DISABLED");
    }

    if log_cfg.polymarket_snapshots {
        let c = cancel.clone();
        let d = log_dir.clone();
        let rx = snapshot_tx.subscribe();
        tokio::spawn(async move { logger::log_polymarket_snapshots(d, rx, c).await; });
    } else {
        info!("Polymarket snapshot logging DISABLED");
    }

    if log_cfg.mock_trades {
        let c = cancel.clone();
        let d = log_dir.clone();
        let rx = trade_tx.subscribe();
        tokio::spawn(async move { logger::log_mock_trades(d, rx, c).await; });
    }

    if log_cfg.evaluations || log_cfg.latency_csv {
        let c = cancel.clone();
        let d = log_dir.clone();
        let rx = eval_tx.subscribe();
        let write_evals = log_cfg.evaluations;
        let write_csv = log_cfg.latency_csv;
        tokio::spawn(async move {
            logger::log_evaluations_conditional(d, rx, write_evals, write_csv, c).await;
        });
    } else {
        info!("Evaluation + latency CSV logging DISABLED");
    }

    if log_cfg.signals {
        let c = cancel.clone();
        let d = log_dir.clone();
        let rx = signal_tx.subscribe();
        tokio::spawn(async move { logger::log_signals(d, rx, c).await; });
    }

    info!(
        "All systems running. Will shut down in {} seconds.",
        config.general.run_duration_secs
    );

    // Run for configured duration, then cancel
    let shutdown_cancel = cancel.clone();
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_secs(config.general.run_duration_secs)) => {
            info!("Run duration elapsed, shutting down...");
        }
        _ = tokio::signal::ctrl_c() => {
            info!("Ctrl-C received, shutting down...");
        }
    }

    shutdown_cancel.cancel();

    // Give tasks a moment to clean up
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    // Print summary
    let st = app_state.read().await;
    let total = st.mock_trades.len();
    let settled = st.mock_trades.iter().filter(|t| t.settled).count();
    let wins = st.mock_trades.iter().filter(|t| t.won == Some(true)).count();
    let directional_count = st.mock_trades.iter().filter(|t| t.strategy == "directional").count();
    let sniping_count = st.mock_trades.iter().filter(|t| t.strategy == "sniping").count();
    info!("=== SESSION SUMMARY ===");
    info!("Total mock trades: {} (directional={}, sniping={})", total, directional_count, sniping_count);
    info!("Settled: {} | Wins: {} | Win rate: {:.1}%",
        settled, wins,
        if settled > 0 { wins as f64 / settled as f64 * 100.0 } else { 0.0 }
    );
    info!("Total PnL: {:.4}", st.pnl);
    info!("=======================");
}
