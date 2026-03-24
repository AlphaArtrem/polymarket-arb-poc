use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub binance: BinanceConfig,
    pub polymarket: PolymarketConfig,
    pub strategy: StrategyConfig,
    pub execution: ExecutionConfig,
    pub latency: LatencyConfig,
    pub general: GeneralConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BinanceConfig {
    pub symbols: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PolymarketConfig {
    pub symbols: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StrategyConfig {
    pub threshold: f64,
    pub min_size: f64,
    pub trade_size: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ExecutionConfig {
    pub taker_fee_bps: f64,
    pub slippage_bps: f64,
    pub max_levels: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LatencyConfig {
    pub simulated_order_delay_ms: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct GeneralConfig {
    pub run_duration_secs: u64,
    pub log_dir: String,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Build the mapping from binance symbol -> polymarket prefix.
    /// e.g. "btcusdt" -> "btc"
    pub fn symbol_mapping(&self) -> Vec<(String, String)> {
        self.binance
            .symbols
            .iter()
            .zip(self.polymarket.symbols.iter())
            .map(|(b, p)| (b.clone(), p.clone()))
            .collect()
    }
}

// Simple error wrapper since we don't want to pull in anyhow crate
mod anyhow {
    pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
}
