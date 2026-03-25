use serde::{Deserialize, Serialize};

/// A tick from the Binance bookTicker stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnderlyingTick {
    pub symbol: String,
    pub bid: f64,
    pub ask: f64,
    pub mid_price: f64,
    pub ts_source: Option<u64>,
    pub ts_receive: u64,
}

/// A single ask level from the order book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AskLevel {
    pub price: f64,
    pub size: f64,
}

/// Current best-bid/ask state for a Polymarket 15-min Up/Down market.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub condition_id: String,
    pub symbol: String,
    pub up_best_bid: Option<f64>,
    pub up_best_ask: Option<f64>,
    pub down_best_bid: Option<f64>,
    pub down_best_ask: Option<f64>,
    pub up_ask_levels: Vec<AskLevel>,
    pub down_ask_levels: Vec<AskLevel>,
    pub window_start: u64,
    pub window_end: u64,
    pub ts_last_update: u64,
}

/// Mapping from symbol to the currently active market.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveMarket {
    pub symbol: String,
    pub condition_id: String,
    pub up_token_id: String,
    pub down_token_id: String,
    pub window_start: u64,
    pub window_end: u64,
}

/// Signal emitted when a directional or sniping trade fires.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectionalSignal {
    pub symbol: String,
    pub strategy: String,
    pub direction: String,
    pub binance_mid: f64,
    pub window_open_price: f64,
    pub price_vs_open_bps: f64,
    pub momentum_short_bps: Option<f64>,
    pub momentum_medium_bps: Option<f64>,
    pub trend_strength: i32,
    pub spike_bps: Option<f64>,
    pub poly_ask_price: f64,
    pub poly_quote_age_ms: u64,
    pub estimated_fair_value: f64,
    pub edge_bps: f64,
    pub ts_signal: u64,
    pub ts_decision_start: u64,
    pub ts_decision_end: u64,
    pub decision_duration_us: u64,
}

/// A single-side paper trade record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockTrade {
    pub id: u64,
    pub symbol: String,
    pub condition_id: String,
    pub strategy: String,
    pub direction: String,
    pub entry_price: f64,
    pub entry_price_with_slippage: f64,
    pub fee_per_share: f64,
    pub trade_size: f64,
    pub total_cost: f64,
    pub estimated_fair_value: f64,
    pub edge_bps: f64,
    pub price_vs_open_bps: f64,
    pub spike_bps: Option<f64>,
    pub poly_quote_age_ms: u64,
    pub ts_binance_receive: u64,
    pub ts_poly_last_update: u64,
    pub ts_decision_start: u64,
    pub ts_decision_end: u64,
    pub ts_mock_order: u64,
    pub simulated_order_delay_ms: u64,
    pub window_start: u64,
    pub window_end: u64,
    pub settled: bool,
    pub resolved_winner: Option<String>,
    pub won: Option<bool>,
    pub payout: Option<f64>,
    pub realized_profit: Option<f64>,
}

/// A strategy evaluation record (logged selectively — near-threshold or trade events).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evaluation {
    pub timestamp: u64,
    pub symbol: String,
    pub binance_mid: f64,
    pub up_best_ask: Option<f64>,
    pub down_best_ask: Option<f64>,
    pub combined_ask: Option<f64>,
    pub price_vs_open_bps: Option<f64>,
    pub spike_bps: Option<f64>,
    pub direction: Option<String>,
    pub strategy: Option<String>,
    pub trade_triggered: bool,
    pub ts_binance_receive: u64,
    pub ts_poly_last_update: Option<u64>,
    pub ts_decision_start: u64,
    pub ts_decision_end: u64,
    pub binance_to_decision_us: u64,
    pub poly_to_decision_us: Option<u64>,
    pub decision_duration_us: u64,
}

/// Resolution event from Polymarket WebSocket.
#[derive(Debug, Clone)]
pub struct MarketResolution {
    pub condition_id: String,
    pub winning_asset_id: String,
    pub winning_outcome: String,
}
