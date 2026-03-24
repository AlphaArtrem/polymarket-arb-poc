# EC2 Deployment & Results Collection Guide
## Polymarket Crypto 15m Arb POC

---

## 1. Launch EC2 Instance

### Region
**eu-west-1 (Dublin)** — required for regulatory reasons + proximity to Polymarket infra.

### Instance Type
**t4g.medium** (2 vCPU ARM Graviton, 4 GB RAM) — ~$0.0336/hr (~$0.81/day)
- ARM/Graviton: Rust compiles natively, ~15-20% cheaper than x86
- 4 GB RAM is plenty for this POC (in-memory state is tiny)
- Burstable CPU is fine — the bot is I/O-bound, not CPU-bound

Alternative if you want zero burst concerns: **c7g.medium** (~$0.0408/hr) — dedicated compute, no throttling.

### AMI
**Amazon Linux 2023** (ARM64) — lightweight, chrony pre-installed.

### Security Group
- Outbound: Allow all (needs to reach Binance WS, Polymarket WS, Gamma API)
- Inbound: SSH only (port 22, your IP)

### Storage
8 GB gp3 is fine. Logs for 24 hours will be <500 MB.

### Key Pair
Create or use an existing SSH key pair.

---

## 2. Initial Setup (SSH in)

```bash
# SSH into your instance
ssh -i your-key.pem ec2-user@<public-ip>

# Update system
sudo dnf update -y

# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source ~/.cargo/env

# Install build essentials (needed for native-tls / openssl)
sudo dnf install -y gcc openssl-devel pkg-config git

# Verify
rustc --version
cargo --version
```

---

## 3. Configure NTP (Critical for Latency Measurement)

Amazon Linux 2023 uses chrony with the local Amazon Time Sync Service by default. Verify it:

```bash
# Check chrony is running
chronyc tracking

# Verify it's using the Amazon NTP endpoint (169.254.169.123)
chronyc sources

# You should see something like:
# ^* 169.254.169.123  1  6  377  ...  +/-  0.3ms
```

If you see the `*` next to `169.254.169.123`, you're good — sub-millisecond sync.

For even better accuracy (optional, Nitro instances only):
```bash
# Check if PTP hardware clock is available
ls /sys/class/ptp/

# If ptp0 exists, configure direct PTP (sub-microsecond sync):
echo 'refclock PHC /dev/ptp0 poll 0 delay 0.000010 prefer' | sudo tee -a /etc/chrony.conf
sudo systemctl restart chronyd
chronyc sources  # Should show PHC0 with * as preferred
```

---

## 4. Clone, Build, Configure

```bash
# Clone the repo
git clone https://github.com/AlphaArtrem/polymarket-arb-poc.git
cd polymarket-arb-poc

# Build release binary (optimized)
cargo build --release
# This will take 2-5 minutes on t4g.medium

# Verify binary exists
ls -la target/release/polymarket-arb-poc
```

### Tune config.toml

The default config is ready to go, but you may want to adjust:

```bash
vim config.toml
```

```toml
[binance]
symbols = ["btcusdt", "ethusdt", "solusdt", "xrpusdt"]

[polymarket]
symbols = ["btc", "eth", "sol", "xrp"]

[strategy]
threshold = 0.95    # Start conservative. We'll tune this after the run.
min_size = 5.0
trade_size = 10.0

[general]
run_duration_secs = 86400  # 24 hours
log_dir = "./logs"
```

---

## 5. Pre-Flight Latency Check

Before the 24-hour run, measure raw network latency:

```bash
# Binance WebSocket endpoint
ping -c 20 stream.binance.com
# Note: avg RTT (expect 1-10ms from Dublin)

# Polymarket CLOB WebSocket
ping -c 20 ws-subscriptions-clob.polymarket.com
# Note: avg RTT

# Polymarket Gamma API
curl -o /dev/null -s -w "DNS: %{time_namelookup}s\nConnect: %{time_connect}s\nTTFB: %{time_starttransfer}s\nTotal: %{time_total}s\n" \
  "https://gamma-api.polymarket.com/events?slug=btc-updown-15m-$(($(date +%s) / 900 * 900))"

# Save these results
echo "=== Pre-flight latency ===" > pre_flight.txt
ping -c 20 stream.binance.com >> pre_flight.txt 2>&1
ping -c 20 ws-subscriptions-clob.polymarket.com >> pre_flight.txt 2>&1
```

---

## 6. Run the 24-Hour Test

```bash
# Create a screen session (persists after SSH disconnect)
screen -S arb

# Run with info logging
RUST_LOG=info ./target/release/polymarket-arb-poc 2>&1 | tee run.log

# Detach from screen: Ctrl-A then D
# Reattach later: screen -r arb
```

Alternative using nohup (simpler):
```bash
mkdir -p logs
RUST_LOG=info nohup ./target/release/polymarket-arb-poc > run.log 2>&1 &
echo $! > pid.txt

# Check it's running
tail -f run.log

# Check status later
cat pid.txt | xargs ps -p
```

### Monitor During Run

```bash
# Watch trades in real-time
tail -f logs/mock_trades.jsonl | python3 -m json.tool

# Count trades so far
wc -l logs/mock_trades.jsonl

# Watch latest evaluations
tail -1 logs/evaluations.jsonl | python3 -m json.tool

# Quick latency check
tail -100 logs/latency_metrics.csv | awk -F, '{sum+=$4; n++} END {print "Avg decision_duration_us:", sum/n}'
```

---

## 7. After the Run — Collect Results

### Package logs for analysis

```bash
# Check log sizes
ls -lh logs/

# Compress all logs
tar czf results-$(date +%Y%m%d).tar.gz logs/ run.log pre_flight.txt

# Quick summary before downloading
echo "=== Quick Summary ==="
echo "Binance ticks: $(wc -l < logs/binance_ticks.jsonl)"
echo "Polymarket snapshots: $(wc -l < logs/polymarket_snapshots.jsonl)"
echo "Mock trades: $(wc -l < logs/mock_trades.jsonl)"
echo "Evaluations: $(wc -l < logs/evaluations.jsonl)"
echo "Latency rows: $(wc -l < logs/latency_metrics.csv)"

# Average combined_ask when trades triggered
python3 -c "
import json
trades = [json.loads(l) for l in open('logs/mock_trades.jsonl')]
if trades:
    avg = sum(t['combined_ask'] for t in trades) / len(trades)
    total_pnl = sum(t['expected_profit'] for t in trades)
    print(f'Total trades: {len(trades)}')
    print(f'Avg combined_ask: {avg:.4f}')
    print(f'Total expected PnL: {total_pnl:.2f}')
    print(f'Avg profit/trade: {total_pnl/len(trades):.4f}')
    by_sym = {}
    for t in trades:
        by_sym.setdefault(t['symbol'], []).append(t)
    for sym, ts in sorted(by_sym.items()):
        print(f'  {sym}: {len(ts)} trades, avg combined_ask={sum(t[\"combined_ask\"] for t in ts)/len(ts):.4f}')
else:
    print('No trades triggered')
"

# Latency distribution
python3 -c "
import csv
with open('logs/latency_metrics.csv') as f:
    reader = csv.DictReader(f)
    durations = []
    for row in reader:
        d = int(row['decision_duration_us'])
        durations.append(d)
if durations:
    durations.sort()
    n = len(durations)
    print(f'Decision latency (us):')
    print(f'  Min:    {durations[0]}')
    print(f'  P50:    {durations[n//2]}')
    print(f'  P95:    {durations[int(n*0.95)]}')
    print(f'  P99:    {durations[int(n*0.99)]}')
    print(f'  Max:    {durations[-1]}')
    print(f'  Count:  {n}')
"
```

### Download to your local machine

```bash
# From your local machine:
scp -i your-key.pem ec2-user@<public-ip>:~/polymarket-arb-poc/results-*.tar.gz .
```

---

## 8. Share Results With Me

Upload these files and I'll analyze them and help tune the strategy:

### Option A: Upload the compressed archive
Just drag-and-drop `results-YYYYMMDD.tar.gz` into the chat. I'll extract and analyze everything.

### Option B: Upload individual files (if the archive is too large)
Priority order:
1. **`logs/mock_trades.jsonl`** — most important (trade opportunities found)
2. **`logs/latency_metrics.csv`** — latency distribution
3. **`logs/evaluations.jsonl`** — all strategy evaluations (may be large)
4. **`run.log`** — console output with session summary
5. **`pre_flight.txt`** — raw ping latencies

### Option C: Paste the summary output
If uploading isn't convenient, run the summary scripts from Section 7 and paste the output. That gives me enough to start tuning.

---

## 9. What I'll Analyze

With your results, I'll compute:

1. **Opportunity frequency** — how many trades per hour, per symbol
2. **Combined ask distribution** — histogram of `up_ask + down_ask` values
3. **Latency budget** — is signal-to-decision within 40-80ms?
4. **PnL projection** — expected profit at different fill assumptions (100%, 80%, 50%)
5. **Threshold tuning** — should we tighten from 0.95 to 0.98? Or loosen?
6. **Time-of-day patterns** — are opportunities clustered around specific hours?
7. **Symbol comparison** — which assets have the best edge?
8. **Next steps** — whether to proceed to live trading, and with what parameters

---

## 10. Cost Estimate

| Item | Cost |
|------|------|
| t4g.medium (24 hours) | ~$0.81 |
| gp3 storage (8 GB) | ~$0.02 |
| Data transfer | ~$0.01 |
| **Total** | **~$0.84** |

Don't forget to **stop or terminate** the instance after collecting results.

---

## Troubleshooting

### Binary won't start
```bash
# Check if port/resource issues
lsof -i :443
# Check logs
tail -50 run.log
```

### No Polymarket data
- Markets rotate every 15 min. The discovery task needs ~2 min to fetch the first market.
- Check `run.log` for "Fetching Gamma API" and "Discovered N markets" messages.
- If you see "Gamma API request failed", check DNS resolution: `dig gamma-api.polymarket.com`

### No trades triggered
- This is actually valuable data. It means `up_ask + down_ask > 0.95` consistently.
- Check `evaluations.jsonl` for the actual `combined_ask` values — we may need to adjust the threshold.
- If `combined_ask` is always ~1.0, that means the market is efficient at your latency (also useful to know).

### Connection drops
- The bot auto-reconnects with exponential backoff.
- Check `run.log` for "Reconnecting" messages and how frequent they are.

### Instance terminated unexpectedly
- Use `screen` or `tmux` to survive SSH disconnects.
- If the instance itself dies, you lose logs. Consider adding an S3 upload cron for safety on longer runs.
