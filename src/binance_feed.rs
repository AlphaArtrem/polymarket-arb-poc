use crate::types::UnderlyingTick;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;
use tokio_tungstenite::connect_async;
use tracing::{error, info, warn};
use url::Url;

/// Raw JSON shape from the combined stream wrapper.
#[derive(Debug, Deserialize)]
struct CombinedStreamMsg {
    #[allow(dead_code)]
    stream: String,
    data: BookTickerRaw,
}

/// Raw bookTicker payload from Binance (all prices are strings).
#[derive(Debug, Deserialize)]
struct BookTickerRaw {
    s: String,  // symbol e.g. "BTCUSDT"
    b: String,  // best bid price
    a: String,  // best ask price
    #[allow(dead_code)]
    #[serde(rename = "B")]
    bid_qty: String,
    #[allow(dead_code)]
    #[serde(rename = "A")]
    ask_qty: String,
}

pub async fn run_binance_feed(
    symbols: Vec<String>,
    tx: broadcast::Sender<UnderlyingTick>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let streams: Vec<String> = symbols
        .iter()
        .map(|s| format!("{}@bookTicker", s))
        .collect();
    let stream_path = streams.join("/");
    let ws_url = format!(
        "wss://stream.binance.com:9443/stream?streams={}",
        stream_path
    );

    let mut backoff = Duration::from_secs(1);

    loop {
        if cancel.is_cancelled() {
            info!("Binance feed shutting down");
            return;
        }

        info!("Connecting to Binance: {}", ws_url);
        let url = match Url::parse(&ws_url) {
            Ok(u) => u,
            Err(e) => {
                error!("Invalid Binance URL: {}", e);
                return;
            }
        };

        let ws_stream = match connect_async(url).await {
            Ok((stream, _)) => {
                info!("Connected to Binance WebSocket");
                backoff = Duration::from_secs(1); // reset backoff on success
                stream
            }
            Err(e) => {
                warn!("Binance connect failed: {}, retrying in {:?}", e, backoff);
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = cancel.cancelled() => return,
                }
                backoff = (backoff * 2).min(Duration::from_secs(30));
                continue;
            }
        };

        let (mut write, mut read) = ws_stream.split();

        // Binance recommends pong in response to ping, tungstenite handles this
        // automatically. We just read messages.
        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(tungstenite::Message::Text(text))) => {
                            match serde_json::from_str::<CombinedStreamMsg>(&text) {
                                Ok(combined) => {
                                    let data = combined.data;
                                    let bid: f64 = match data.b.parse() {
                                        Ok(v) => v,
                                        Err(_) => continue,
                                    };
                                    let ask: f64 = match data.a.parse() {
                                        Ok(v) => v,
                                        Err(_) => continue,
                                    };
                                    let now = SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap()
                                        .as_millis() as u64;
                                    let tick = UnderlyingTick {
                                        symbol: data.s.to_lowercase(),
                                        bid,
                                        ask,
                                        mid_price: (bid + ask) / 2.0,
                                        ts_source: None,
                                        ts_receive: now,
                                    };
                                    let _ = tx.send(tick);
                                }
                                Err(e) => {
                                    warn!("Failed to parse Binance msg: {}", e);
                                }
                            }
                        }
                        Some(Ok(tungstenite::Message::Ping(payload))) => {
                            let _ = write.send(tungstenite::Message::Pong(payload)).await;
                        }
                        Some(Ok(tungstenite::Message::Close(_))) => {
                            warn!("Binance WS closed by server");
                            break;
                        }
                        Some(Err(e)) => {
                            warn!("Binance WS error: {}", e);
                            break;
                        }
                        None => {
                            warn!("Binance WS stream ended");
                            break;
                        }
                        _ => {}
                    }
                }
                _ = cancel.cancelled() => {
                    info!("Binance feed cancelled");
                    return;
                }
            }
        }

        // Reconnect with backoff
        warn!("Reconnecting to Binance in {:?}", backoff);
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = cancel.cancelled() => return,
        }
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}
