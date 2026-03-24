# Polymarket Crypto 15m Arb – Paper Trading POC

Rust proof-of-concept for detecting and paper-trading arbitrage opportunities on Polymarket's 15-minute crypto Up/Down markets (BTC, ETH, SOL, XRP).

**No real orders are sent.** This is purely for latency measurement and checking that profitable opportunities exist at your latency.

## Architecture

```
┌───────────────┐    broadcast     ┌──────────────────┐
│ BinanceFeed   │───────────────→ │ StrategyEngine   │
│ (bookTicker)  │                  │ (threshold arb)  │
└───────────────┘                  │                  │
                                   │  reads state ↓   │
┌───────────────┐    broadcast     │                  │──→ MockTrade log
│ PolymarketFeed│───────────────→ │                  │──→ Evaluation log
│ (CLOB WS)     │                  └──────────────────┘──→ Latency CSV
│ + Discovery   │
│   (Gamma API) │    broadcast     ┌──────────────────┐
│               │───────────────→ │ Settlement       │
└───────────────┘                  │ (market_resolved)│
                                   └──────────────────┘
```

### Components

| Module | File | Role |
|--------|------|------|
| Config | `src/config.rs` | Loads `config.toml` (symbols, thresholds, run duration) |
| Types | `src/types.rs` | `UnderlyingTick`, `MarketSnapshot`, `ActiveMarket`, `MockTrade`, `Evaluation` |
| BinanceFeed | `src/binance_feed.rs` | Combined bookTicker WS for 4 symbols, reconnect with backoff |
| PolymarketFeed | `src/polymarket_feed.rs` | Gamma API market discovery + CLOB WS (price_change, best_bid_ask, book, market_resolved) |
| StateStore | `src/state.rs` | `Arc<RwLock<AppState>>` for underlying prices, market snapshots, trades, PnL |
| Strategy | `src/strategy.rs` | Threshold-based arb detection on every Binance tick + settlement on resolution |
| Logger | `src/logger.rs` | JSONL and CSV file sinks for ticks, snapshots, trades, evaluations, latency |
| Main | `src/main.rs` | Wires everything, graceful shutdown via CancellationToken + Ctrl-C |

## Strategy Logic

On each Binance tick for a symbol:
1. Read current Polymarket snapshot for the corresponding 15-min Up/Down market
2. If `up_best_ask + down_best_ask <= threshold` (default 0.95):
   - Log a mock pair trade (buy Up at ask + buy Down at ask)
   - Expected profit = `(1.0 - combined_ask) * trade_size` per trade
3. Record all timestamps for latency analysis

At market resolution, settle all open trades for that market: winner pays 1.0, loser pays 0.0.

## Latency Measurements

Every strategy evaluation records:
- `binance_to_decision_us`: WebSocket receive → decision start (microseconds)
- `poly_to_decision_us`: Last Polymarket update → decision start (microseconds)
- `decision_duration_us`: Decision computation time (microseconds)
- `ts_mock_order`: Timestamp of mock order creation

## Quick Start

```bash
# Build
cargo build --release

# Edit config if needed
vim config.toml

# Run (default: 24 hours)
RUST_LOG=info ./target/release/polymarket-arb-poc

# Or run for a shorter test
# Edit config.toml: run_duration_secs = 300  (5 minutes)
```

## Configuration

```toml
[binance]
symbols = ["btcusdt", "ethusdt", "solusdt", "xrpusdt"]

[polymarket]
symbols = ["btc", "eth", "sol", "xrp"]

[strategy]
threshold = 0.95    # combined Up_ask + Down_ask must be <= this
min_size = 5.0      # minimum order size (Polymarket minimum)
trade_size = 10.0   # fixed paper trade size per side

[general]
run_duration_secs = 86400  # 24 hours
log_dir = "./logs"
```

## Output Files

After a run, `./logs/` will contain:

| File | Format | Contents |
|------|--------|----------|
| `binance_ticks.jsonl` | JSONL | Every Binance bookTicker update |
| `polymarket_snapshots.jsonl` | JSONL | Every Polymarket best bid/ask update |
| `mock_trades.jsonl` | JSONL | All triggered mock trades with full timestamp chain |
| `evaluations.jsonl` | JSONL | Every strategy evaluation (trade or no-trade) |
| `latency_metrics.csv` | CSV | `timestamp, binance_to_decision_us, poly_to_decision_us, decision_duration_us` |

## Deployment (Dublin VPS)

```bash
# On the VPS:
git clone <this-repo>
cd polymarket-arb-poc

# Install Rust if needed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build release binary
cargo build --release

# Run in screen/tmux
screen -S arb
RUST_LOG=info ./target/release/polymarket-arb-poc
# Ctrl-A D to detach

# After 24 hours, collect logs from ./logs/
```

## Market Discovery

Markets rotate every 15 minutes. The slug pattern is:
- `{symbol}-updown-15m-{window_start_unix}`
- Example: `btc-updown-15m-1774381500`

The discovery task pre-fetches the next window's market ~2 minutes before it opens, subscribes to new token IDs on the CLOB WebSocket, and unsubscribes from expired ones.

## Known Limitations (POC)

- No real order placement (paper only)
- Fixed trade size (no Kelly/position sizing)
- No Bayes/KL-divergence signal — just simple threshold
- No VPS NTP calibration (affects latency measurements)
- 1-day run is for validation only, not parameter tuning
- Resolution uses Chainlink BTC/USD, not Binance — slight price difference expected
