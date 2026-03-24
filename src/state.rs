use crate::types::{ActiveMarket, MarketSnapshot, MockTrade, UnderlyingTick};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct AppState {
    pub underlying: HashMap<String, UnderlyingTick>,
    pub markets: HashMap<String, MarketSnapshot>,
    pub active_markets: HashMap<String, ActiveMarket>,
    pub mock_trades: Vec<MockTrade>,
    pub pnl: f64,
    pub next_trade_id: u64,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            underlying: HashMap::new(),
            markets: HashMap::new(),
            active_markets: HashMap::new(),
            mock_trades: Vec::new(),
            pnl: 0.0,
            next_trade_id: 1,
        }
    }
}

pub type SharedState = Arc<RwLock<AppState>>;

pub fn create_state() -> SharedState {
    Arc::new(RwLock::new(AppState::new()))
}
