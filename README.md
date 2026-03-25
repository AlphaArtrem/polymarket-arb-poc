# Polymarket Crypto 15m – Directional Edge + Stale-Price Sniping POC

Rust proof-of-concept for detecting and paper-trading directional opportunities on Polymarket's 15-minute crypto Up/Down markets (BTC, ETH, SOL, XRP).

**No real orders are sent.** This is purely for signal validation and latency measurement.

## Strategies

### Strategy 1: Directional Speed Edge

Detects when Binance BTC price moves significantly from the 15-minute window open price. If the move exceeds a threshold (default 30 bps), buys the likely winning side on Polymarket before the market maker reprices.

- Signal: `price_vs_open_bps` = (current_mid / window_open - 1) * 10000
- Direction: "Up" if positive, "Down" if negative
- Entry: Buy target side at Polymarket ask if ask < estimated fair value and ask < max_entry_price
- Fair value: linear approximation `0.5 + price_vs_open_bps * sensitivity`, clamped [0.05, 0.95]

### Strategy 2: Stale-Price Sniping

Detects rapid price spikes on Binance (default >20 bps in 5 seconds) where Polymarket quotes haven't caught up. Buys the underpriced side.

- Signal: `spike_bps` = price change over spike_window_secs
- Direction: same as spike direction — spike up means buy "Up"
- Entry: same fair value check as directional

Both strategies can fire independently. Directional has cooldown to prevent overtrading on the same signal.

## Architecture

```
┌───────────────┐    broadcast     ┌──────────────────┐
│ BinanceFeed   │───────────────→ │ StrategyEngine   │
│ (bookTicker)  │                  │ (directional +   │
└───────────────┘                  │  sniping)        │
                                   │                  │
┌───────────────┐   poly_state     │  PriceTracker    │──→ MockTrade log
│ PolymarketFeed│───────────────→ │  (signals.rs)    │──→ Signal log
│ (CLOB WS)     │                  └──────────────────┘──→ Evaluation log
│ + Discovery   │
│   (Gamma API) │    broadcast     ┌──────────────────┐
│               │───────────────→ │ Settlement       │
└───────────────┘                  │ (single-side PnL)│
                                   └──────────────────┘
```

### Components

| Module | File | Role |
|--------|------|------|
| Config | `src/config.rs` | Loads `config.toml` (directional, sniping, fees, latency) |
| Types | `src/types.rs` | `UnderlyingTick`, `MarketSnapshot`, `MockTrade`, `DirectionalSignal`, `Evaluation` |
| Signals | `src/signals.rs` | `PriceTracker` — rolling tick buffer, momentum, spike detection |
| BinanceFeed | `src/binance_feed.rs` | Combined bookTicker WS for 4 symbols, reconnect with backoff |
| PolymarketFeed | `src/polymarket_feed.rs` | Gamma API market discovery + CLOB WS |
| StateStore | `src/state.rs` | `Arc<RwLock<AppState>>` for trades, PnL |
| Strategy | `src/strategy.rs` | Directional + sniping decision logic, settlement |
| Logger | `src/logger.rs` | JSONL and CSV file sinks |
| Main | `src/main.rs` | Wires everything, graceful shutdown |

## Settlement

Single-side trades: if we bought "Up" and "Up" wins, payout = 1.0/share. If "Down" wins, payout = 0.0.

- realized_profit = payout - total_cost
- total_cost = entry_price * trade_size + fee_per_share * trade_size
- fee_per_share = rate * (price * (1 - price))^exponent (Polymarket crypto fee formula)

## Quick Start

```bash
cargo build --release
vim config.toml  # adjust if needed
RUST_LOG=info ./target/release/polymarket-arb-poc
```

## Configuration

```toml
[binance]
symbols = ["btcusdt", "ethusdt", "solusdt", "xrpusdt"]

[polymarket]
symbols = ["btc", "eth", "sol", "xrp"]

[directional]
enabled = true
directional_threshold_bps = 30.0    # min price_vs_open to trigger
max_entry_price = 0.85              # don't buy if ask > this
momentum_window_secs = 30           # ring buffer for momentum
min_time_into_window_secs = 60      # skip first 60s of window
max_time_before_close_secs = 120    # skip last 2 min of window
cooldown_secs = 30                  # min seconds between trades
fair_value_sensitivity = 0.005
trade_size = 10.0

[sniping]
enabled = true
spike_threshold_bps = 20.0          # min spike to trigger
spike_window_secs = 5               # spike measurement window
max_entry_price = 0.85
staleness_max_ms = 2000
fair_value_sensitivity = 0.005
trade_size = 10.0

[fees]
rate = 0.25
exponent = 2

[latency]
simulated_order_delay_ms = 10

[general]
run_duration_secs = 86400
log_dir = "./logs"
```

## Output Files

| File | Format | Contents |
|------|--------|----------|
| `binance_ticks.jsonl` | JSONL | Every Binance bookTicker update |
| `polymarket_snapshots.jsonl` | JSONL | Every Polymarket best bid/ask update |
| `mock_trades.jsonl` | JSONL | All triggered mock trades (single-side) |
| `signals.jsonl` | JSONL | Every DirectionalSignal (when trade fires) |
| `evaluations.jsonl` | JSONL | Near-threshold and trade evaluations |
| `latency_metrics.csv` | CSV | Latency distribution |

## Key Analysis Metrics

After a 24-hour run:
1. **Signal frequency**: how often directional / sniping signals fire per symbol
2. **Entry prices**: distribution of Polymarket ask prices at entry
3. **Price-vs-open at signal**: are we catching real trends or noise
4. **Quote staleness**: how old Polymarket quotes are when sniping fires
5. **Win rate**: after resolution, what % of trades won
6. **PnL distribution**: profit per trade, by strategy and symbol
7. **Edge analysis**: at what bps threshold does win rate exceed 50%?
