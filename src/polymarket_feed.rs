use crate::types::{ActiveMarket, AskLevel, MarketResolution, MarketSnapshot};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, RwLock};
use tracing::{error, info, warn};
use url::Url;

// ── Gamma API response types ──

#[derive(Debug, Deserialize)]
struct GammaEvent {
    markets: Option<Vec<GammaMarket>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GammaMarket {
    condition_id: Option<String>,
    clob_token_ids: Option<String>, // JSON array as string: "[\"id1\",\"id2\"]"
    outcomes: Option<String>,       // JSON array as string: "[\"Up\",\"Down\"]"
    #[allow(dead_code)]
    accepting_orders: Option<bool>,
}

// ── CLOB WebSocket event types ──

#[derive(Debug, Deserialize)]
struct ClobEvent {
    event_type: Option<String>,
    asset_id: Option<String>,
    market: Option<String>,
    // book event fields
    bids: Option<Vec<OrderLevel>>,
    asks: Option<Vec<OrderLevel>>,
    // price_change fields
    price_changes: Option<Vec<PriceChange>>,
    // best_bid_ask fields
    best_bid: Option<String>,
    best_ask: Option<String>,
    // market_resolved fields
    winning_asset_id: Option<String>,
    winning_outcome: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OrderLevel {
    price: String,
    size: String,
}

#[derive(Debug, Deserialize)]
struct PriceChange {
    asset_id: Option<String>,
    best_bid: Option<String>,
    best_ask: Option<String>,
}

/// Shared state for token->market mapping.
pub struct PolymarketState {
    /// token_id -> (symbol, "up"/"down")
    pub token_map: HashMap<String, (String, String)>,
    /// condition_id -> MarketSnapshot
    pub snapshots: HashMap<String, MarketSnapshot>,
    /// symbol -> ActiveMarket
    pub active_markets: HashMap<String, ActiveMarket>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn current_window_start() -> u64 {
    let now = now_ms() / 1000;
    (now / 900) * 900
}

fn next_window_start() -> u64 {
    current_window_start() + 900
}

/// Fetch market info from Gamma API for a given symbol and window timestamp.
async fn fetch_market(
    client: &reqwest::Client,
    symbol: &str,
    window_ts: u64,
) -> Option<ActiveMarket> {
    let slug = format!("{}-updown-15m-{}", symbol, window_ts);
    let url = format!("https://gamma-api.polymarket.com/events?slug={}", slug);
    info!("Fetching Gamma API: {}", url);

    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!("Gamma API request failed for {}: {}", slug, e);
            return None;
        }
    };

    let events: Vec<GammaEvent> = match resp.json().await {
        Ok(e) => e,
        Err(e) => {
            warn!("Gamma API parse failed for {}: {}", slug, e);
            return None;
        }
    };

    let event = events.into_iter().next()?;
    let market = event.markets?.into_iter().next()?;
    let condition_id = market.condition_id?;
    let token_ids_str = market.clob_token_ids?;
    let token_ids: Vec<String> = serde_json::from_str(&token_ids_str).ok()?;
    if token_ids.len() < 2 {
        warn!("Expected 2 token IDs for {}, got {}", slug, token_ids.len());
        return None;
    }

    // outcomes should be ["Up", "Down"] — first token is Up, second is Down
    let _outcomes: Vec<String> = market
        .outcomes
        .and_then(|o| serde_json::from_str(&o).ok())
        .unwrap_or_else(|| vec!["Up".into(), "Down".into()]);

    Some(ActiveMarket {
        symbol: symbol.to_string(),
        condition_id,
        up_token_id: token_ids[0].clone(),
        down_token_id: token_ids[1].clone(),
        window_start: window_ts,
        window_end: window_ts + 900,
    })
}

/// Periodically discover upcoming markets and update shared state.
pub async fn run_market_discovery(
    symbols: Vec<String>,
    state: Arc<RwLock<PolymarketState>>,
    // Channel to signal the WS task about new subscriptions
    sub_tx: tokio::sync::mpsc::Sender<SubscriptionUpdate>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let client = reqwest::Client::new();

    loop {
        if cancel.is_cancelled() {
            return;
        }

        // Determine which window to discover
        let now_secs = now_ms() / 1000;
        let current_start = (now_secs / 900) * 900;
        let time_into_window = now_secs - current_start;

        // Discover current window if we don't have it yet, otherwise next window
        let target_ts = if time_into_window < 780 {
            // More than 2 min left — ensure we have current window
            current_start
        } else {
            // Less than 2 min left — pre-fetch next window
            current_start + 900
        };

        let mut new_markets = Vec::new();
        for sym in &symbols {
            if let Some(market) = fetch_market(&client, sym, target_ts).await {
                new_markets.push(market);
            }
        }

        if !new_markets.is_empty() {
            let mut st = state.write().await;
            let mut old_tokens = Vec::new();
            let mut new_tokens = Vec::new();

            for market in &new_markets {
                // Collect old tokens to unsubscribe
                if let Some(old) = st.active_markets.get(&market.symbol) {
                    if old.condition_id != market.condition_id {
                        let old_up = old.up_token_id.clone();
                        let old_down = old.down_token_id.clone();
                        let old_cid = old.condition_id.clone();
                        old_tokens.push(old_up.clone());
                        old_tokens.push(old_down.clone());
                        // Remove old mappings
                        st.token_map.remove(&old_up);
                        st.token_map.remove(&old_down);
                        st.snapshots.remove(&old_cid);
                    }
                }

                // Add new mappings
                st.token_map.insert(
                    market.up_token_id.clone(),
                    (market.symbol.clone(), "up".to_string()),
                );
                st.token_map.insert(
                    market.down_token_id.clone(),
                    (market.symbol.clone(), "down".to_string()),
                );
                st.snapshots.insert(
                    market.condition_id.clone(),
                    MarketSnapshot {
                        condition_id: market.condition_id.clone(),
                        symbol: market.symbol.clone(),
                        up_best_bid: None,
                        up_best_ask: None,
                        down_best_bid: None,
                        down_best_ask: None,
                        up_ask_levels: Vec::new(),
                        down_ask_levels: Vec::new(),
                        window_start: market.window_start,
                        window_end: market.window_end,
                        ts_last_update: now_ms(),
                    },
                );
                st.active_markets
                    .insert(market.symbol.clone(), market.clone());

                new_tokens.push(market.up_token_id.clone());
                new_tokens.push(market.down_token_id.clone());
            }
            drop(st);

            // Signal WS to update subscriptions
            if !old_tokens.is_empty() {
                let _ = sub_tx
                    .send(SubscriptionUpdate::Unsubscribe(old_tokens))
                    .await;
            }
            if !new_tokens.is_empty() {
                let _ = sub_tx
                    .send(SubscriptionUpdate::Subscribe(new_tokens))
                    .await;
            }

            info!(
                "Discovered {} markets for window {}",
                new_markets.len(),
                target_ts
            );
        }

        // Sleep until ~2 min before next window boundary
        let now_secs2 = now_ms() / 1000;
        let current_start2 = (now_secs2 / 900) * 900;
        let next_discovery = current_start2 + 900 - 120; // 2 min before next window
        let sleep_secs = if next_discovery > now_secs2 {
            next_discovery - now_secs2
        } else {
            60 // fallback: check again in 60s
        };

        info!("Next market discovery in {}s", sleep_secs);
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(sleep_secs)) => {}
            _ = cancel.cancelled() => return,
        }
    }
}

#[derive(Debug)]
pub enum SubscriptionUpdate {
    Subscribe(Vec<String>),
    Unsubscribe(Vec<String>),
}

/// Run the Polymarket CLOB WebSocket connection.
pub async fn run_clob_ws(
    state: Arc<RwLock<PolymarketState>>,
    snapshot_tx: broadcast::Sender<MarketSnapshot>,
    resolution_tx: broadcast::Sender<MarketResolution>,
    mut sub_rx: tokio::sync::mpsc::Receiver<SubscriptionUpdate>,
    max_levels: usize,
    cancel: tokio_util::sync::CancellationToken,
) {
    let ws_url = "wss://ws-subscriptions-clob.polymarket.com/ws/market";
    let mut backoff = Duration::from_secs(1);

    loop {
        if cancel.is_cancelled() {
            return;
        }

        info!("Connecting to Polymarket CLOB WS: {}", ws_url);
        let url = match Url::parse(ws_url) {
            Ok(u) => u,
            Err(e) => {
                error!("Invalid Polymarket URL: {}", e);
                return;
            }
        };

        let ws_stream = match tokio_tungstenite::connect_async(url).await {
            Ok((stream, _)) => {
                info!("Connected to Polymarket CLOB WebSocket");
                backoff = Duration::from_secs(1);
                stream
            }
            Err(e) => {
                warn!("Polymarket CLOB connect failed: {}, retrying in {:?}", e, backoff);
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = cancel.cancelled() => return,
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };

        let (mut write, mut read) = ws_stream.split();

        // Send initial subscription with all currently known tokens
        {
            let st = state.read().await;
            let all_tokens: Vec<String> = st.token_map.keys().cloned().collect();
            if !all_tokens.is_empty() {
                let sub_msg = json!({
                    "assets_ids": all_tokens,
                    "type": "market",
                    "custom_feature_enabled": true
                });
                if let Err(e) = write
                    .send(tungstenite::Message::Text(sub_msg.to_string()))
                    .await
                {
                    warn!("Failed to send initial subscription: {}", e);
                }
            }
        }

        // Heartbeat interval
        let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
        heartbeat.tick().await; // consume immediate tick

        loop {
            tokio::select! {
                _ = heartbeat.tick() => {
                    if let Err(e) = write.send(tungstenite::Message::Text("PING".to_string())).await {
                        warn!("Failed to send PING: {}", e);
                        break;
                    }
                }
                msg = read.next() => {
                    match msg {
                        Some(Ok(tungstenite::Message::Text(text))) => {
                            if text == "PONG" {
                                continue;
                            }
                            handle_clob_message(
                                &text,
                                &state,
                                &snapshot_tx,
                                &resolution_tx,
                                max_levels,
                            ).await;
                        }
                        Some(Ok(tungstenite::Message::Close(_))) => {
                            warn!("Polymarket CLOB WS closed");
                            break;
                        }
                        Some(Err(e)) => {
                            warn!("Polymarket CLOB WS error: {}", e);
                            break;
                        }
                        None => {
                            warn!("Polymarket CLOB WS stream ended");
                            break;
                        }
                        _ => {}
                    }
                }
                sub_update = sub_rx.recv() => {
                    match sub_update {
                        Some(SubscriptionUpdate::Subscribe(tokens)) => {
                            let msg = json!({
                                "assets_ids": tokens,
                                "operation": "subscribe",
                                "custom_feature_enabled": true
                            });
                            let _ = write.send(tungstenite::Message::Text(msg.to_string())).await;
                            info!("Subscribed to {} new tokens", tokens.len());
                        }
                        Some(SubscriptionUpdate::Unsubscribe(tokens)) => {
                            let msg = json!({
                                "assets_ids": tokens,
                                "operation": "unsubscribe"
                            });
                            let _ = write.send(tungstenite::Message::Text(msg.to_string())).await;
                            info!("Unsubscribed from {} tokens", tokens.len());
                        }
                        None => {
                            warn!("Subscription channel closed");
                            break;
                        }
                    }
                }
                _ = cancel.cancelled() => {
                    info!("Polymarket CLOB WS cancelled");
                    return;
                }
            }
        }

        warn!("Reconnecting to Polymarket CLOB WS in {:?}", backoff);
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = cancel.cancelled() => return,
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn handle_clob_message(
    text: &str,
    state: &Arc<RwLock<PolymarketState>>,
    snapshot_tx: &broadcast::Sender<MarketSnapshot>,
    resolution_tx: &broadcast::Sender<MarketResolution>,
    max_levels: usize,
) {
    let event: ClobEvent = match serde_json::from_str(text) {
        Ok(e) => e,
        Err(_) => return, // ignore unparseable messages
    };

    let event_type = match &event.event_type {
        Some(t) => t.as_str(),
        None => return,
    };

    let now = now_ms();

    match event_type {
        "price_change" => {
            if let Some(changes) = &event.price_changes {
                let mut st = state.write().await;
                for change in changes {
                    if let Some(asset_id) = &change.asset_id {
                        update_bid_ask(
                            &mut st,
                            asset_id,
                            change.best_bid.as_deref(),
                            change.best_ask.as_deref(),
                            now,
                            snapshot_tx,
                        );
                    }
                }
            }
        }
        "best_bid_ask" => {
            if let Some(asset_id) = &event.asset_id {
                let mut st = state.write().await;
                update_bid_ask(
                    &mut st,
                    asset_id,
                    event.best_bid.as_deref(),
                    event.best_ask.as_deref(),
                    now,
                    snapshot_tx,
                );
            }
        }
        "book" => {
            if let Some(asset_id) = &event.asset_id {
                // Extract best bid (highest) and best ask (lowest)
                let best_bid = event.bids.as_ref().and_then(|bids| {
                    bids.iter()
                        .filter_map(|l| l.price.parse::<f64>().ok())
                        .fold(None, |max: Option<f64>, v| {
                            Some(max.map_or(v, |m| m.max(v)))
                        })
                });
                let best_ask = event.asks.as_ref().and_then(|asks| {
                    asks.iter()
                        .filter_map(|l| l.price.parse::<f64>().ok())
                        .fold(None, |min: Option<f64>, v| {
                            Some(min.map_or(v, |m| m.min(v)))
                        })
                });

                // Extract ask levels sorted by price ascending, up to max_levels
                let ask_levels: Vec<AskLevel> = if let Some(asks) = &event.asks {
                    let mut levels: Vec<AskLevel> = asks
                        .iter()
                        .filter_map(|l| {
                            let price = l.price.parse::<f64>().ok()?;
                            let size = l.size.parse::<f64>().ok()?;
                            Some(AskLevel { price, size })
                        })
                        .collect();
                    levels.sort_by(|a, b| a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal));
                    levels.truncate(max_levels);
                    levels
                } else {
                    Vec::new()
                };

                let mut st = state.write().await;

                // Store ask levels for this side
                if let Some((symbol, side)) = st.token_map.get(asset_id).cloned() {
                    if let Some(active) = st.active_markets.get(&symbol) {
                        let cid = active.condition_id.clone();
                        if let Some(snap) = st.snapshots.get_mut(&cid) {
                            if side == "up" {
                                snap.up_ask_levels = ask_levels;
                            } else {
                                snap.down_ask_levels = ask_levels;
                            }
                        }
                    }
                }

                update_bid_ask_values(
                    &mut st,
                    asset_id,
                    best_bid,
                    best_ask,
                    now,
                    snapshot_tx,
                );
            }
        }
        "market_resolved" => {
            if let (Some(winning_asset_id), Some(winning_outcome)) =
                (&event.winning_asset_id, &event.winning_outcome)
            {
                let condition_id = event.market.unwrap_or_default();
                let _ = resolution_tx.send(MarketResolution {
                    condition_id,
                    winning_asset_id: winning_asset_id.clone(),
                    winning_outcome: winning_outcome.clone(),
                });
            }
        }
        _ => {}
    }
}

fn update_bid_ask(
    st: &mut PolymarketState,
    asset_id: &str,
    best_bid: Option<&str>,
    best_ask: Option<&str>,
    now: u64,
    snapshot_tx: &broadcast::Sender<MarketSnapshot>,
) {
    let bid_val = best_bid.and_then(|s| s.parse::<f64>().ok());
    let ask_val = best_ask.and_then(|s| s.parse::<f64>().ok());
    update_bid_ask_values(st, asset_id, bid_val, ask_val, now, snapshot_tx);
}

fn update_bid_ask_values(
    st: &mut PolymarketState,
    asset_id: &str,
    best_bid: Option<f64>,
    best_ask: Option<f64>,
    now: u64,
    snapshot_tx: &broadcast::Sender<MarketSnapshot>,
) {
    let (symbol, side) = match st.token_map.get(asset_id) {
        Some(v) => v.clone(),
        None => return,
    };

    let active = match st.active_markets.get(&symbol) {
        Some(a) => a,
        None => return,
    };
    let cid = active.condition_id.clone();

    if let Some(snap) = st.snapshots.get_mut(&cid) {
        if side == "up" {
            if let Some(b) = best_bid {
                snap.up_best_bid = Some(b);
            }
            if let Some(a) = best_ask {
                snap.up_best_ask = Some(a);
            }
        } else {
            if let Some(b) = best_bid {
                snap.down_best_bid = Some(b);
            }
            if let Some(a) = best_ask {
                snap.down_best_ask = Some(a);
            }
        }
        snap.ts_last_update = now;
        let _ = snapshot_tx.send(snap.clone());
    }
}
