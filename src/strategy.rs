use crate::config::{DirectionalConfig, FeesConfig, LatencyConfig, SnipingConfig};
use crate::polymarket_feed::PolymarketState;
use crate::signals::PriceTracker;
use crate::state::SharedState;
use crate::types::{DirectionalSignal, Evaluation, MockTrade, UnderlyingTick};
use std::collections::HashMap;
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

/// Polymarket fee per share: rate * (price * (1 - price))^exponent
fn polymarket_fee(price: f64, rate: f64, exponent: u32) -> f64 {
    rate * (price * (1.0 - price)).powi(exponent as i32)
}

/// Linear fair value estimate: 0.5 + price_vs_open_bps * sensitivity, clamped to [0.05, 0.95].
fn estimate_fair_value_up(price_vs_open_bps: f64, sensitivity: f64) -> f64 {
    (0.5 + price_vs_open_bps * sensitivity).clamp(0.05, 0.95)
}

pub async fn run_strategy(
    directional_cfg: DirectionalConfig,
    sniping_cfg: SnipingConfig,
    fees_cfg: FeesConfig,
    latency_cfg: LatencyConfig,
    mut tick_rx: broadcast::Receiver<UnderlyingTick>,
    poly_state: Arc<RwLock<PolymarketState>>,
    app_state: SharedState,
    eval_tx: broadcast::Sender<Evaluation>,
    trade_tx: broadcast::Sender<MockTrade>,
    signal_tx: broadcast::Sender<DirectionalSignal>,
    cancel: tokio_util::sync::CancellationToken,
) {
    info!(
        "Strategy engine started (directional={}, sniping={}, fee_rate={}, fee_exp={}, delay={}ms)",
        directional_cfg.enabled,
        sniping_cfg.enabled,
        fees_cfg.rate,
        fees_cfg.exponent,
        latency_cfg.simulated_order_delay_ms,
    );

    let max_buffer = directional_cfg.momentum_window_secs.max(60);
    let mut tracker = PriceTracker::new(max_buffer);

    // Cooldown: (symbol, direction) -> last_trade_ts_ms
    let mut cooldowns: HashMap<(String, String), u64> = HashMap::new();

    // Track last known window per symbol to detect changes
    let mut known_windows: HashMap<String, (u64, u64)> = HashMap::new();

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
                let ts_now = tick.ts_receive;

                // Update PriceTracker window from poly_state if changed
                {
                    let ps = poly_state.read().await;
                    if let Some(active) = ps.active_markets.get(&poly_sym) {
                        let current_window = (active.window_start, active.window_end);
                        let changed = known_windows
                            .get(&poly_sym)
                            .map_or(true, |w| *w != current_window);
                        if changed {
                            tracker.set_window(&poly_sym, active.window_start, active.window_end);
                            known_windows.insert(poly_sym.clone(), current_window);
                        }
                    }
                }

                // Update tracker with new tick
                tracker.on_tick(&poly_sym, tick.mid_price, ts_now);

                // Compute signals
                let price_vs_open = tracker.price_vs_open_bps(&poly_sym, tick.mid_price);
                let momentum_short = tracker.momentum_bps(&poly_sym, tick.mid_price, 5, ts_now);
                let momentum_medium = tracker.momentum_bps(&poly_sym, tick.mid_price, 15, ts_now);
                let spike = tracker.spike_bps(&poly_sym, tick.mid_price, sniping_cfg.spike_window_secs, ts_now);
                let trend = tracker.trend_strength(&poly_sym);
                let time_into = tracker.time_into_window_secs(&poly_sym, ts_now);
                let time_before_close = tracker.time_before_close_secs(&poly_sym, ts_now);

                // Read Polymarket snapshot
                let ps = poly_state.read().await;
                let active = ps.active_markets.get(&poly_sym).cloned();
                let snapshot = active
                    .as_ref()
                    .and_then(|a| ps.snapshots.get(&a.condition_id))
                    .cloned();
                drop(ps);

                let (snap_ref, condition_id, window_start, window_end) = match (&snapshot, &active) {
                    (Some(s), Some(a)) => (s, a.condition_id.clone(), a.window_start, a.window_end),
                    _ => {
                        // No market data — emit minimal evaluation
                        let ts_decision_end = now_ms();
                        let elapsed = decision_start.elapsed();
                        let eval = Evaluation {
                            timestamp: ts_decision_start,
                            symbol: poly_sym.clone(),
                            binance_mid: tick.mid_price,
                            up_best_ask: None,
                            down_best_ask: None,
                            combined_ask: None,
                            price_vs_open_bps: price_vs_open,
                            spike_bps: spike,
                            direction: None,
                            strategy: None,
                            trade_triggered: false,
                            ts_binance_receive: tick.ts_receive,
                            ts_poly_last_update: None,
                            ts_decision_start,
                            ts_decision_end,
                            binance_to_decision_us: ts_decision_start.saturating_sub(tick.ts_receive) * 1000,
                            poly_to_decision_us: None,
                            decision_duration_us: elapsed.as_micros() as u64,
                        };
                        let _ = eval_tx.send(eval);
                        continue;
                    }
                };

                let ts_poly = snap_ref.ts_last_update;
                let quote_age_ms = ts_now.saturating_sub(ts_poly);

                // Check timing constraints
                let in_window = time_into.map_or(false, |t| t >= directional_cfg.min_time_into_window_secs);
                let not_too_late = time_before_close.map_or(false, |t| t >= directional_cfg.max_time_before_close_secs);
                let timing_ok = in_window && not_too_late;

                let pvo_bps = price_vs_open.unwrap_or(0.0);
                let open_price = tracker.window_open_price(&poly_sym).unwrap_or(0.0);

                // Sensitivity for fair value
                let dir_sensitivity = directional_cfg.fair_value_sensitivity;
                let snipe_sensitivity = sniping_cfg.fair_value_sensitivity;

                let mut trade_triggered = false;
                let mut triggered_strategy: Option<String> = None;
                let mut triggered_direction: Option<String> = None;

                // ── Directional strategy check ──
                if directional_cfg.enabled && timing_ok && pvo_bps.abs() > directional_cfg.directional_threshold_bps {
                    let direction = if pvo_bps > 0.0 { "Up" } else { "Down" };
                    let fair_value_up = estimate_fair_value_up(pvo_bps, dir_sensitivity);
                    let (target_ask, fair_value) = if direction == "Up" {
                        (snap_ref.up_best_ask, fair_value_up)
                    } else {
                        (snap_ref.down_best_ask, 1.0 - fair_value_up)
                    };

                    if let Some(ask) = target_ask {
                        if ask < directional_cfg.max_entry_price && ask < fair_value {
                            // Check cooldown
                            let cd_key = (poly_sym.clone(), direction.to_string());
                            let cooldown_ok = cooldowns
                                .get(&cd_key)
                                .map_or(true, |&last| ts_now.saturating_sub(last) > directional_cfg.cooldown_secs * 1000);

                            if cooldown_ok {
                                let ts_decision_end = now_ms();
                                let elapsed = decision_start.elapsed();
                                let ts_mock_order = ts_decision_end + latency_cfg.simulated_order_delay_ms;
                                let fee = polymarket_fee(ask, fees_cfg.rate, fees_cfg.exponent);
                                let total_cost = ask * directional_cfg.trade_size + fee * directional_cfg.trade_size;
                                let edge_bps = (fair_value - ask) * 10_000.0;

                                let signal = DirectionalSignal {
                                    symbol: poly_sym.clone(),
                                    strategy: "directional".to_string(),
                                    direction: direction.to_string(),
                                    binance_mid: tick.mid_price,
                                    window_open_price: open_price,
                                    price_vs_open_bps: pvo_bps,
                                    momentum_short_bps: momentum_short,
                                    momentum_medium_bps: momentum_medium,
                                    trend_strength: trend,
                                    spike_bps: spike,
                                    poly_ask_price: ask,
                                    poly_quote_age_ms: quote_age_ms,
                                    estimated_fair_value: fair_value,
                                    edge_bps,
                                    ts_signal: ts_now,
                                    ts_decision_start,
                                    ts_decision_end,
                                    decision_duration_us: elapsed.as_micros() as u64,
                                };
                                let _ = signal_tx.send(signal);

                                let mut st = app_state.write().await;
                                let trade_id = st.next_trade_id;
                                st.next_trade_id += 1;

                                let mock_trade = MockTrade {
                                    id: trade_id,
                                    symbol: poly_sym.clone(),
                                    condition_id: condition_id.clone(),
                                    strategy: "directional".to_string(),
                                    direction: direction.to_string(),
                                    entry_price: ask,
                                    entry_price_with_slippage: ask,
                                    fee_per_share: fee,
                                    trade_size: directional_cfg.trade_size,
                                    total_cost,
                                    estimated_fair_value: fair_value,
                                    edge_bps,
                                    price_vs_open_bps: pvo_bps,
                                    spike_bps: spike,
                                    poly_quote_age_ms: quote_age_ms,
                                    ts_binance_receive: tick.ts_receive,
                                    ts_poly_last_update: ts_poly,
                                    ts_decision_start,
                                    ts_decision_end,
                                    ts_mock_order,
                                    simulated_order_delay_ms: latency_cfg.simulated_order_delay_ms,
                                    window_start,
                                    window_end,
                                    settled: false,
                                    resolved_winner: None,
                                    won: None,
                                    payout: None,
                                    realized_profit: None,
                                };

                                info!(
                                    "DIRECTIONAL #{}: {} {} ask={:.4} fv={:.4} edge={:.1}bps pvo={:.1}bps",
                                    trade_id, poly_sym, direction, ask, fair_value, edge_bps, pvo_bps
                                );

                                st.mock_trades.push(mock_trade.clone());
                                drop(st);

                                let _ = trade_tx.send(mock_trade);
                                cooldowns.insert(cd_key, ts_now);
                                trade_triggered = true;
                                triggered_strategy = Some("directional".to_string());
                                triggered_direction = Some(direction.to_string());
                            }
                        }
                    }
                }

                // ── Sniping strategy check ──
                if sniping_cfg.enabled && !trade_triggered {
                    if let Some(spike_val) = spike {
                        if spike_val.abs() > sniping_cfg.spike_threshold_bps {
                            let direction = if spike_val > 0.0 { "Up" } else { "Down" };
                            let fair_value_up = estimate_fair_value_up(pvo_bps, snipe_sensitivity);
                            let (target_ask, fair_value) = if direction == "Up" {
                                (snap_ref.up_best_ask, fair_value_up)
                            } else {
                                (snap_ref.down_best_ask, 1.0 - fair_value_up)
                            };

                            if let Some(ask) = target_ask {
                                if ask < sniping_cfg.max_entry_price && ask < fair_value {
                                    let ts_decision_end = now_ms();
                                    let elapsed = decision_start.elapsed();
                                    let ts_mock_order = ts_decision_end + latency_cfg.simulated_order_delay_ms;
                                    let fee = polymarket_fee(ask, fees_cfg.rate, fees_cfg.exponent);
                                    let total_cost = ask * sniping_cfg.trade_size + fee * sniping_cfg.trade_size;
                                    let edge_bps = (fair_value - ask) * 10_000.0;

                                    let signal = DirectionalSignal {
                                        symbol: poly_sym.clone(),
                                        strategy: "sniping".to_string(),
                                        direction: direction.to_string(),
                                        binance_mid: tick.mid_price,
                                        window_open_price: open_price,
                                        price_vs_open_bps: pvo_bps,
                                        momentum_short_bps: momentum_short,
                                        momentum_medium_bps: momentum_medium,
                                        trend_strength: trend,
                                        spike_bps: Some(spike_val),
                                        poly_ask_price: ask,
                                        poly_quote_age_ms: quote_age_ms,
                                        estimated_fair_value: fair_value,
                                        edge_bps,
                                        ts_signal: ts_now,
                                        ts_decision_start,
                                        ts_decision_end,
                                        decision_duration_us: elapsed.as_micros() as u64,
                                    };
                                    let _ = signal_tx.send(signal);

                                    let mut st = app_state.write().await;
                                    let trade_id = st.next_trade_id;
                                    st.next_trade_id += 1;

                                    let mock_trade = MockTrade {
                                        id: trade_id,
                                        symbol: poly_sym.clone(),
                                        condition_id: condition_id.clone(),
                                        strategy: "sniping".to_string(),
                                        direction: direction.to_string(),
                                        entry_price: ask,
                                        entry_price_with_slippage: ask,
                                        fee_per_share: fee,
                                        trade_size: sniping_cfg.trade_size,
                                        total_cost,
                                        estimated_fair_value: fair_value,
                                        edge_bps,
                                        price_vs_open_bps: pvo_bps,
                                        spike_bps: Some(spike_val),
                                        poly_quote_age_ms: quote_age_ms,
                                        ts_binance_receive: tick.ts_receive,
                                        ts_poly_last_update: ts_poly,
                                        ts_decision_start,
                                        ts_decision_end,
                                        ts_mock_order,
                                        simulated_order_delay_ms: latency_cfg.simulated_order_delay_ms,
                                        window_start,
                                        window_end,
                                        settled: false,
                                        resolved_winner: None,
                                        won: None,
                                        payout: None,
                                        realized_profit: None,
                                    };

                                    info!(
                                        "SNIPING #{}: {} {} ask={:.4} fv={:.4} edge={:.1}bps spike={:.1}bps age={}ms",
                                        trade_id, poly_sym, direction, ask, fair_value, edge_bps, spike_val, quote_age_ms
                                    );

                                    st.mock_trades.push(mock_trade.clone());
                                    drop(st);

                                    let _ = trade_tx.send(mock_trade);
                                    trade_triggered = true;
                                    triggered_strategy = Some("sniping".to_string());
                                    triggered_direction = Some(direction.to_string());
                                }
                            }
                        }
                    }
                }

                // Log evaluation only when near threshold or trade triggered
                let should_log = trade_triggered || pvo_bps.abs() > 15.0 || spike.map_or(false, |s| s.abs() > 10.0);
                if should_log {
                    let ts_decision_end = now_ms();
                    let elapsed = decision_start.elapsed();
                    let combined = match (snap_ref.up_best_ask, snap_ref.down_best_ask) {
                        (Some(u), Some(d)) => Some(u + d),
                        _ => None,
                    };
                    let eval = Evaluation {
                        timestamp: ts_decision_start,
                        symbol: poly_sym.clone(),
                        binance_mid: tick.mid_price,
                        up_best_ask: snap_ref.up_best_ask,
                        down_best_ask: snap_ref.down_best_ask,
                        combined_ask: combined,
                        price_vs_open_bps: price_vs_open,
                        spike_bps: spike,
                        direction: triggered_direction,
                        strategy: triggered_strategy,
                        trade_triggered,
                        ts_binance_receive: tick.ts_receive,
                        ts_poly_last_update: Some(ts_poly),
                        ts_decision_start,
                        ts_decision_end,
                        binance_to_decision_us: ts_decision_start.saturating_sub(tick.ts_receive) * 1000,
                        poly_to_decision_us: Some(ts_decision_start.saturating_sub(ts_poly) * 1000),
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

/// Handle market resolutions and settle open trades (single-side).
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
                        trade.resolved_winner = Some(resolution.winning_outcome.clone());
                        let won = trade.direction == resolution.winning_outcome;
                        trade.won = Some(won);

                        if won {
                            let payout = 1.0 * trade.trade_size;
                            let profit = payout - trade.total_cost;
                            trade.payout = Some(payout);
                            trade.realized_profit = Some(profit);
                            total_pnl_delta += profit;
                            settled_ids.push((trade.id, trade.strategy.clone(), trade.direction.clone(), profit, true));
                        } else {
                            let loss = 0.0 - trade.total_cost;
                            trade.payout = Some(0.0);
                            trade.realized_profit = Some(loss);
                            total_pnl_delta += loss;
                            settled_ids.push((trade.id, trade.strategy.clone(), trade.direction.clone(), loss, false));
                        }
                    }
                }

                st.pnl += total_pnl_delta;
                let current_pnl = st.pnl;
                let wins = settled_ids.iter().filter(|(_, _, _, _, w)| *w).count();
                let total = settled_ids.len();

                for (tid, strat, dir, pnl, won) in &settled_ids {
                    info!(
                        "SETTLED #{}: {} {} {} won={} pnl={:.4} total_pnl={:.4}",
                        tid, strat, dir, resolution.winning_outcome, won, pnl, current_pnl
                    );
                }
                if total > 0 {
                    info!(
                        "Settlement batch: {}/{} won, delta={:.4}, session_pnl={:.4}",
                        wins, total, total_pnl_delta, current_pnl
                    );
                }
            }
            _ = cancel.cancelled() => return,
        }
    }
}
