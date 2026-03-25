use std::collections::{HashMap, VecDeque};

struct SymbolState {
    window_open_price: Option<f64>,
    window_start_ts: u64,
    window_end_ts: u64,
    tick_buffer: VecDeque<(u64, f64)>, // (timestamp_ms, mid_price)
    last_direction: Option<bool>,       // true = up, false = down
    consecutive_count: i32,
}

pub struct PriceTracker {
    symbols: HashMap<String, SymbolState>,
    max_buffer_secs: u64,
}

impl PriceTracker {
    pub fn new(max_buffer_secs: u64) -> Self {
        Self {
            symbols: HashMap::new(),
            max_buffer_secs,
        }
    }

    fn get_or_create(&mut self, symbol: &str) -> &mut SymbolState {
        self.symbols.entry(symbol.to_string()).or_insert_with(|| SymbolState {
            window_open_price: None,
            window_start_ts: 0,
            window_end_ts: 0,
            tick_buffer: VecDeque::new(),
            last_direction: None,
            consecutive_count: 0,
        })
    }

    /// Add a new tick, prune old ticks, track consecutive direction.
    pub fn on_tick(&mut self, symbol: &str, mid: f64, ts_ms: u64) {
        let max_age_ms = self.max_buffer_secs * 1000;
        let state = self.get_or_create(symbol);

        // Track consecutive direction by comparing to previous tick
        if let Some(&(_, prev_mid)) = state.tick_buffer.back() {
            if mid > prev_mid {
                // Up tick
                match state.last_direction {
                    Some(true) => state.consecutive_count += 1,
                    _ => {
                        state.last_direction = Some(true);
                        state.consecutive_count = 1;
                    }
                }
            } else if mid < prev_mid {
                // Down tick
                match state.last_direction {
                    Some(false) => state.consecutive_count -= 1,
                    _ => {
                        state.last_direction = Some(false);
                        state.consecutive_count = -1;
                    }
                }
            }
            // If mid == prev_mid, no change to direction tracking
        }

        state.tick_buffer.push_back((ts_ms, mid));

        // Prune old ticks beyond max_buffer_secs
        while let Some(&(oldest_ts, _)) = state.tick_buffer.front() {
            if ts_ms.saturating_sub(oldest_ts) > max_age_ms {
                state.tick_buffer.pop_front();
            } else {
                break;
            }
        }

        // Set open price on first tick within window
        if state.window_open_price.is_none() && state.window_start_ts > 0 && ts_ms >= state.window_start_ts * 1000 {
            state.window_open_price = Some(mid);
        }
    }

    /// Set the window start/end for a symbol. Clears open price so it gets set on next tick.
    pub fn set_window(&mut self, symbol: &str, start_ts: u64, end_ts: u64) {
        let state = self.get_or_create(symbol);
        if state.window_start_ts != start_ts || state.window_end_ts != end_ts {
            state.window_start_ts = start_ts;
            state.window_end_ts = end_ts;
            state.window_open_price = None;
        }
    }

    /// Explicitly set the open price for a symbol.
    pub fn set_open_price(&mut self, symbol: &str, price: f64) {
        let state = self.get_or_create(symbol);
        state.window_open_price = Some(price);
    }

    /// Returns (current_mid / open_price - 1.0) * 10_000.0 in bps.
    pub fn price_vs_open_bps(&self, symbol: &str, current_mid: f64) -> Option<f64> {
        let state = self.symbols.get(symbol)?;
        let open = state.window_open_price?;
        if open == 0.0 {
            return None;
        }
        Some((current_mid / open - 1.0) * 10_000.0)
    }

    /// Returns momentum over the last `secs` seconds in bps.
    /// Finds the oldest tick within `secs` ago, returns (current_mid / old_mid - 1.0) * 10_000.
    pub fn momentum_bps(&self, symbol: &str, current_mid: f64, secs: u64, ts_now: u64) -> Option<f64> {
        let state = self.symbols.get(symbol)?;
        let cutoff = ts_now.saturating_sub(secs * 1000);
        // Find oldest tick that is >= cutoff
        let old_mid = state
            .tick_buffer
            .iter()
            .find(|(ts, _)| *ts >= cutoff)
            .map(|(_, mid)| *mid)?;
        if old_mid == 0.0 {
            return None;
        }
        Some((current_mid / old_mid - 1.0) * 10_000.0)
    }

    /// Same as momentum_bps but semantically for spike detection (shorter window).
    pub fn spike_bps(&self, symbol: &str, current_mid: f64, secs: u64, ts_now: u64) -> Option<f64> {
        self.momentum_bps(symbol, current_mid, secs, ts_now)
    }

    /// Returns consecutive_count: positive for up, negative for down.
    pub fn trend_strength(&self, symbol: &str) -> i32 {
        self.symbols.get(symbol).map_or(0, |s| s.consecutive_count)
    }

    /// How many seconds since window start.
    pub fn time_into_window_secs(&self, symbol: &str, ts_now: u64) -> Option<u64> {
        let state = self.symbols.get(symbol)?;
        if state.window_start_ts == 0 {
            return None;
        }
        let now_secs = ts_now / 1000;
        if now_secs >= state.window_start_ts {
            Some(now_secs - state.window_start_ts)
        } else {
            Some(0)
        }
    }

    /// How many seconds until window end.
    pub fn time_before_close_secs(&self, symbol: &str, ts_now: u64) -> Option<u64> {
        let state = self.symbols.get(symbol)?;
        if state.window_end_ts == 0 {
            return None;
        }
        let now_secs = ts_now / 1000;
        if state.window_end_ts > now_secs {
            Some(state.window_end_ts - now_secs)
        } else {
            Some(0)
        }
    }

    /// Get the window open price for a symbol.
    pub fn window_open_price(&self, symbol: &str) -> Option<f64> {
        self.symbols.get(symbol)?.window_open_price
    }
}
