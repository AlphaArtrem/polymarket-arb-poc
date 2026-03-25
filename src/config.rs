use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub binance: BinanceConfig,
    pub polymarket: PolymarketConfig,
    pub directional: DirectionalConfig,
    pub sniping: SnipingConfig,
    pub fees: FeesConfig,
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
pub struct DirectionalConfig {
    pub enabled: bool,
    pub directional_threshold_bps: f64,
    pub max_entry_price: f64,
    pub momentum_window_secs: u64,
    pub min_time_into_window_secs: u64,
    pub max_time_before_close_secs: u64,
    pub cooldown_secs: u64,
    pub fair_value_sensitivity: f64,
    pub trade_size: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SnipingConfig {
    pub enabled: bool,
    pub spike_threshold_bps: f64,
    pub spike_window_secs: u64,
    pub max_entry_price: f64,
    pub staleness_max_ms: u64,
    pub fair_value_sensitivity: f64,
    pub trade_size: f64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FeesConfig {
    pub rate: f64,
    pub exponent: u32,
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
