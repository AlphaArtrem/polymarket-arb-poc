use crate::polymarket_feed::PolymarketState;
use crate::state::SharedState;
use crate::types::{Evaluation, MarketSnapshot, MockTrade, UnderlyingTick};
use std::collections::{HashMap, VecDeque};
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

/// Rolling buffer of recent snapshots per symbol, used to find the snapshot
/// closest to ts_mock_order (= ts_decision_end + simulated_order_delay_ms).
const MAX_SNAPSHOT_HISTORY: usize = 500;

pub async fn run_strategy(
    threshold: f64,
    trade_size: f64,
    taker_fee_bps: f64,
    slippage_bps: f64,
    simulated_order_delay_ms: u64,
    mut tick_rx: broadcast::Receiver<UnderlyingTick>,
    mut snapshot_rx: broadcast::Receiver<MarketSnapshot>,
    poly_state: Arc<RwLock<PolymarketState>>,
    app_state: SharedState,
    eval_tx: broadcast::Sender<Evaluation>,
    trade_tx: broadcast::Sender<MockTrade>,
    cancel: tokio_util::sync::CancellationToken,
) {
    info!(
        "Strategy engine started (threshold={}, taker_fee_bps={}, slippage_bps={}, simulated_order_delay_ms={})",
        threshold, taker_fee_bps, slippage_bps, simulated_order_delay_ms
    );

    // Rolling buffer: symbol -> VecDeque<(ts_receive, MarketSnapshot)>
    let mut snapshot_history: HashMap<String, VecDeque<(u64, MarketSnapshot)>> = HashMap::new();

    loop {
        tokio::select! {
            // Populate snapshot buffer from broadcast
            snap = snapshot_rx.recv() => {
                match snap {
                    Ok(s) => {
                        let deque = snapshot_history
                            .entry(s.symbol.clone())
                            .or_insert_with(VecDeque::new);
                        let ts = s.ts_last_update;
                        deque.push_back((ts, s));
                        if deque.len() > MAX_SNAPSHOT_HISTORY {
                            deque.pop_front();
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Snapshot buffer lagged {} messages", n);
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
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

                // Look up the current market snapshot for initial check
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
                                combined_exec: None,
                                expected_profit_gross: None,
                                expected_profit_net: None,
                                edge_positive: false,
                                up_depth_available: None,
                                down_depth_available: None,
                                simulated_order_delay_ms,
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

                if trade_triggered {
                    let up_ask_price = up_ask.unwrap();
                    let down_ask_price = down_ask.unwrap();
                    let combined_ask = combined.unwrap();

                    // Compute ts_mock_order and find the snapshot at that time
                    let ts_mock_order = ts_decision_end + simulated_order_delay_ms;

                    // Find the latest snapshot with ts <= ts_mock_order from the buffer.
                    // If no buffered snapshot, fall back to latest known from poly_state.
                    let exec_snapshot: Option<MarketSnapshot> = snapshot_history
                        .get(&poly_sym)
                        .and_then(|deque| {
                            deque.iter().rev().find(|(ts, _)| *ts <= ts_mock_order).map(|(_, s)| s.clone())
                        });

                    let fallback_snapshot: Option<MarketSnapshot> = if exec_snapshot.is_none() {
                        let ps2 = poly_state.read().await;
                        let s = ps2.active_markets.get(&poly_sym)
                            .and_then(|a| ps2.snapshots.get(&a.condition_id))
                            .cloned();
                        drop(ps2);
                        s
                    } else {
                        None
                    };

                    let used_snapshot = exec_snapshot.as_ref().or(fallback_snapshot.as_ref());

                    let exec_up_ask = used_snapshot
                        .and_then(|s| s.up_best_ask)
                        .unwrap_or(up_ask_price);
                    let exec_down_ask = used_snapshot
                        .and_then(|s| s.down_best_ask)
                        .unwrap_or(down_ask_price);

                    // Check available depth
                    let empty_levels = Vec::new();
                    let exec_up_levels = used_snapshot.map(|s| &s.up_ask_levels).unwrap_or(&empty_levels);
                    let exec_down_levels = used_snapshot.map(|s| &s.down_ask_levels).unwrap_or(&empty_levels);
                    let up_depth: f64 = exec_up_levels.iter().map(|l| l.size).sum();
                    let down_depth: f64 = exec_down_levels.iter().map(|l| l.size).sum();

                    // Apply slippage to execution prices
                    let up_exec_price = exec_up_ask * (1.0 + slippage_bps / 10_000.0);
                    let down_exec_price = exec_down_ask * (1.0 + slippage_bps / 10_000.0);
                    let combined_exec = up_exec_price + down_exec_price;

                    // Compute fees
                    let up_fee = trade_size * up_exec_price * taker_fee_bps / 10_000.0;
                    let down_fee = trade_size * down_exec_price * taker_fee_bps / 10_000.0;
                    let total_fee = up_fee + down_fee;

                    // Compute profitability
                    let expected_profit_gross = (1.0 - combined_exec) * trade_size;
                    let expected_profit_net = expected_profit_gross - total_fee;
                    let edge_positive = expected_profit_net > 0.0;

                    // Build evaluation with full details
                    let eval = Evaluation {
                        timestamp: ts_decision_start,
                        symbol: poly_sym.clone(),
                        binance_mid: tick.mid_price,
                        up_best_ask: up_ask,
                        down_best_ask: down_ask,
                        combined_ask: combined,
                        combined_exec: Some(combined_exec),
                        expected_profit_gross: Some(expected_profit_gross),
                        expected_profit_net: Some(expected_profit_net),
                        edge_positive,
                        up_depth_available: Some(up_depth),
                        down_depth_available: Some(down_depth),
                        simulated_order_delay_ms,
                        trade_triggered: true,
                        ts_binance_receive: tick.ts_receive,
                        ts_poly_last_update: ts_poly,
                        ts_decision_start,
                        ts_decision_end,
                        binance_to_decision_us: ts_decision_start.saturating_sub(tick.ts_receive) * 1000,
                        poly_to_decision_us: ts_poly.map(|t| ts_decision_start.saturating_sub(t) * 1000),
                        decision_duration_us: elapsed.as_micros() as u64,
                    };
                    let _ = eval_tx.send(eval);

                    // Log trade regardless of edge_positive
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
                        up_exec_price,
                        down_exec_price,
                        combined_exec,
                        up_fee,
                        down_fee,
                        total_fee,
                        expected_profit_gross,
                        expected_profit_net,
                        edge_positive,
                        simulated_order_delay_ms,
                        ts_binance_receive: tick.ts_receive,
                        ts_poly_last_update: ts_poly.unwrap_or(0),
                        ts_decision_start,
                        ts_decision_end,
                        ts_mock_order,
                        window_start,
                        window_end,
                        settled: false,
                        winning_outcome: None,
                        realized_profit: None,
                        resolved_winner: None,
                    };

                    info!(
                        "TRADE #{}: {} combined_ask={:.4} exec={:.4} gross={:.4} net={:.4} edge={}",
                        trade_id, poly_sym, combined_ask, combined_exec,
                        expected_profit_gross, expected_profit_net, edge_positive
                    );

                    st.mock_trades.push(mock_trade.clone());
                    drop(st);

                    let _ = trade_tx.send(mock_trade);
                } else {
                    // No trade triggered — simpler evaluation
                    let eval = Evaluation {
                        timestamp: ts_decision_start,
                        symbol: poly_sym.clone(),
                        binance_mid: tick.mid_price,
                        up_best_ask: up_ask,
                        down_best_ask: down_ask,
                        combined_ask: combined,
                        combined_exec: None,
                        expected_profit_gross: None,
                        expected_profit_net: None,
                        edge_positive: false,
                        up_depth_available: None,
                        down_depth_available: None,
                        simulated_order_delay_ms,
                        trade_triggered: false,
                        ts_binance_receive: tick.ts_receive,
                        ts_poly_last_update: ts_poly,
                        ts_decision_start,
                        ts_decision_end,
                        binance_to_decision_us: ts_decision_start.saturating_sub(tick.ts_receive) * 1000,
                        poly_to_decision_us: ts_poly.map(|t| ts_decision_start.saturating_sub(t) * 1000),
                        decision_duration_us: elapsed.as_micros() as u64,
                    };
                    let _ = eval_tx.send(eval);
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
                        trade.resolved_winner = Some(resolution.winning_outcome.clone());

                        // Payout: winner side pays 1.0 per share
                        // Entry cost uses execution prices (with slippage)
                        let payout = trade.trade_size * 1.0;
                        let entry_cost = (trade.up_exec_price + trade.down_exec_price) * trade.trade_size;
                        let realized_profit = payout - entry_cost - trade.total_fee;
                        trade.realized_profit = Some(realized_profit);
                        total_pnl_delta += realized_profit;
                        settled_ids.push((trade.id, realized_profit));
                    }
                }
                st.pnl += total_pnl_delta;
                let current_pnl = st.pnl;
                for (tid, pnl) in settled_ids {
                    info!(
                        "SETTLED trade #{}: outcome={} realized_pnl={:.4} total_pnl={:.4}",
                        tid, resolution.winning_outcome, pnl, current_pnl
                    );
                }
            }
            _ = cancel.cancelled() => return,
        }
    }
}
