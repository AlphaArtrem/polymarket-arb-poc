use crate::polymarket_feed::PolymarketState;
use crate::state::SharedState;
use crate::types::{Evaluation, MockTrade, UnderlyingTick};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, RwLock};
use tracing::info;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

pub async fn run_strategy(
    threshold: f64,
    trade_size: f64,
    mut tick_rx: broadcast::Receiver<UnderlyingTick>,
    poly_state: Arc<RwLock<PolymarketState>>,
    app_state: SharedState,
    eval_tx: broadcast::Sender<Evaluation>,
    trade_tx: broadcast::Sender<MockTrade>,
    cancel: tokio_util::sync::CancellationToken,
) {
    info!("Strategy engine started (threshold={})", threshold);

    loop {
        tokio::select! {
            tick = tick_rx.recv() => {
                let tick = match tick {
                    Ok(t) => t,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Strategy lagged {} ticks", n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("Tick channel closed, strategy stopping");
                        return;
                    }
                };

                let decision_start = Instant::now();
                let ts_decision_start = now_ms();

                // Map binance symbol (e.g. "btcusdt") to polymarket symbol (e.g. "btc")
                let poly_sym = tick.symbol.replace("usdt", "");

                // Look up the current market snapshot
                let ps = poly_state.read().await;
                let active = ps.active_markets.get(&poly_sym);
                let snapshot = active.and_then(|a| ps.snapshots.get(&a.condition_id));

                let (up_ask, down_ask, ts_poly, window_start, window_end, condition_id) =
                    match snapshot {
                        Some(snap) => (
                            snap.up_best_ask,
                            snap.down_best_ask,
                            Some(snap.ts_last_update),
                            snap.window_start,
                            snap.window_end,
                            snap.condition_id.clone(),
                        ),
                        None => {
                            let ts_decision_end = now_ms();
                            let elapsed = decision_start.elapsed();
                            let eval = Evaluation {
                                timestamp: ts_decision_start,
                                symbol: poly_sym.clone(),
                                binance_mid: tick.mid_price,
                                up_best_ask: None,
                                down_best_ask: None,
                                combined_ask: None,
                                trade_triggered: false,
                                ts_binance_receive: tick.ts_receive,
                                ts_poly_last_update: None,
                                ts_decision_start,
                                ts_decision_end,
                                binance_to_decision_us: (ts_decision_start - tick.ts_receive) * 1000,
                                poly_to_decision_us: None,
                                decision_duration_us: elapsed.as_micros() as u64,
                            };
                            let _ = eval_tx.send(eval);
                            continue;
                        }
                    };
                drop(ps);

                let combined = match (up_ask, down_ask) {
                    (Some(u), Some(d)) => Some(u + d),
                    _ => None,
                };

                let trade_triggered = combined.map_or(false, |c| c <= threshold);

                let ts_decision_end = now_ms();
                let elapsed = decision_start.elapsed();

                // Build evaluation
                let eval = Evaluation {
                    timestamp: ts_decision_start,
                    symbol: poly_sym.clone(),
                    binance_mid: tick.mid_price,
                    up_best_ask: up_ask,
                    down_best_ask: down_ask,
                    combined_ask: combined,
                    trade_triggered,
                    ts_binance_receive: tick.ts_receive,
                    ts_poly_last_update: ts_poly,
                    ts_decision_start,
                    ts_decision_end,
                    binance_to_decision_us: ts_decision_start.saturating_sub(tick.ts_receive) * 1000,
                    poly_to_decision_us: ts_poly.map(|t| ts_decision_start.saturating_sub(t) * 1000),
                    decision_duration_us: elapsed.as_micros() as u64,
                };
                let _ = eval_tx.send(eval);

                if trade_triggered {
                    let up_ask_price = up_ask.unwrap();
                    let down_ask_price = down_ask.unwrap();
                    let combined_ask = combined.unwrap();
                    let expected_profit = (1.0 - combined_ask) * trade_size;

                    let mut st = app_state.write().await;
                    let trade_id = st.next_trade_id;
                    st.next_trade_id += 1;

                    let mock_trade = MockTrade {
                        id: trade_id,
                        symbol: poly_sym.clone(),
                        condition_id: condition_id.clone(),
                        up_ask_price,
                        down_ask_price,
                        combined_ask,
                        trade_size,
                        expected_profit,
                        ts_binance_receive: tick.ts_receive,
                        ts_poly_last_update: ts_poly.unwrap_or(0),
                        ts_decision_start,
                        ts_decision_end,
                        ts_mock_order: now_ms(),
                        window_start,
                        window_end,
                        settled: false,
                        winning_outcome: None,
                        actual_pnl: None,
                    };

                    info!(
                        "TRADE #{}: {} combined_ask={:.4} expected_profit={:.4}",
                        trade_id, poly_sym, combined_ask, expected_profit
                    );

                    st.mock_trades.push(mock_trade.clone());
                    drop(st);

                    let _ = trade_tx.send(mock_trade);
                }
            }
            _ = cancel.cancelled() => {
                info!("Strategy engine shutting down");
                return;
            }
        }
    }
}

/// Handle market resolutions and settle open trades.
pub async fn run_settlement(
    mut resolution_rx: broadcast::Receiver<crate::types::MarketResolution>,
    app_state: SharedState,
    cancel: tokio_util::sync::CancellationToken,
) {
    loop {
        tokio::select! {
            res = resolution_rx.recv() => {
                let resolution = match res {
                    Ok(r) => r,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Settlement lagged {} resolutions", n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };

                let mut st = app_state.write().await;
                let mut total_pnl_delta = 0.0_f64;
                let mut settled_ids = Vec::new();
                for trade in st.mock_trades.iter_mut() {
                    if trade.condition_id == resolution.condition_id && !trade.settled {
                        trade.settled = true;
                        trade.winning_outcome = Some(resolution.winning_outcome.clone());
                        // Both Up and Down were bought, winner pays 1.0 per share
                        // Cost was combined_ask * trade_size, payout is 1.0 * trade_size
                        let pnl = (1.0 - trade.combined_ask) * trade.trade_size;
                        trade.actual_pnl = Some(pnl);
                        total_pnl_delta += pnl;
                        settled_ids.push((trade.id, pnl));
                    }
                }
                st.pnl += total_pnl_delta;
                let current_pnl = st.pnl;
                for (tid, pnl) in settled_ids {
                    info!(
                        "SETTLED trade #{}: outcome={} pnl={:.4} total_pnl={:.4}",
                        tid, resolution.winning_outcome, pnl, current_pnl
                    );
                }
            }
            _ = cancel.cancelled() => return,
        }
    }
}
