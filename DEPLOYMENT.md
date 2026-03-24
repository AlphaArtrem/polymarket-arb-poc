# EC2 Deployment & Results Collection Guide
## Polymarket Crypto 15m Arb POC

Workflow: build locally, push binary + config to EC2 via `scp`, run on EC2. No git or Rust toolchain needed on the VPS.

---

## 1. Launch EC2 Instance

### Region
**eu-west-1 (Dublin)** — required for regulatory reasons + proximity to Polymarket infra.

### Instance Type
**t4g.medium** (2 vCPU ARM Graviton, 4 GB RAM) — ~$0.0336/hr (~$0.81/day)
- ARM/Graviton: ~15-20% cheaper than x86
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

## 2. Build Locally and Push to EC2

### Prerequisites (your local machine)
- Rust toolchain installed
- If your local machine is x86 (Mac Intel / Linux x86) and EC2 is ARM (t4g/c7g), you need to cross-compile

### Option A: Local machine is ARM (Mac Apple Silicon / Linux ARM)

```bash
# Clone and build locally
git clone https://github.com/AlphaArtrem/polymarket-arb-poc.git
cd polymarket-arb-poc
cargo build --release

# Push binary + config to EC2
scp -i your-key.pem target/release/polymarket-arb-poc ec2-user@<public-ip>:~/
scp -i your-key.pem config.toml ec2-user@<public-ip>:~/
```

### Option B: Local machine is x86 — cross-compile for ARM

```bash
# Install the ARM target
rustup target add aarch64-unknown-linux-gnu

# On macOS, install the cross-compilation linker
brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu
# Or use 'cross' (Docker-based, works on any OS):
cargo install cross

# Clone and cross-compile
git clone https://github.com/AlphaArtrem/polymarket-arb-poc.git
cd polymarket-arb-poc

# Using cross (easiest, requires Docker):
cross build --release --target aarch64-unknown-linux-gnu

# Push the ARM binary + config to EC2
scp -i your-key.pem target/aarch64-unknown-linux-gnu/release/polymarket-arb-poc ec2-user@<public-ip>:~/
scp -i your-key.pem config.toml ec2-user@<public-ip>:~/
```

### Option C: Use an x86 EC2 instance instead

If cross-compiling is a hassle, just launch a **t3.medium** (x86) instead of t4g. Then build natively on your x86 local machine:

```bash
cargo build --release
scp -i your-key.pem target/release/polymarket-arb-poc ec2-user@<public-ip>:~/
scp -i your-key.pem config.toml ec2-user@<public-ip>:~/
```

---

## 3. EC2 Setup (SSH in)

Only minimal setup needed — no Rust, no git.

```bash
ssh -i your-key.pem ec2-user@<public-ip>

# Make binary executable (scp preserves permissions, but just in case)
chmod +x ~/polymarket-arb-poc

# Create logs directory
mkdir -p ~/logs

# Verify binary runs
~/polymarket-arb-poc --help 2>&1 || echo "Binary is ready (no --help flag, will run with config.toml)"
```

---

## 4. Configure NTP (Critical for Latency Measurement)

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

## 5. Tune Config (Optional)

Edit `config.toml` on the EC2 instance if you want to adjust parameters:

```bash
vi ~/config.toml
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

[execution]
taker_fee_bps = 30        # fee per side in basis points
slippage_bps = 10         # extra price impact in bps
max_levels = 3            # book depth levels to consider

[latency]
simulated_order_delay_ms = 10  # simulated network delay to exchange

[general]
run_duration_secs = 86400  # 24 hours
log_dir = "./logs"
```

Or edit it locally before pushing:
```bash
# From your local machine — edit, then push updated config
scp -i your-key.pem config.toml ec2-user@<public-ip>:~/
```

---

## 6. Pre-Flight Latency Check

Before the 24-hour run, measure raw network latency from EC2:

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
echo "=== Pre-flight latency ===" > ~/pre_flight.txt
ping -c 20 stream.binance.com >> ~/pre_flight.txt 2>&1
ping -c 20 ws-subscriptions-clob.polymarket.com >> ~/pre_flight.txt 2>&1
```

---

## 7. Run the 24-Hour Test

```bash
cd ~

# Create a screen session (persists after SSH disconnect)
screen -S arb

# Run with info logging
RUST_LOG=info ./polymarket-arb-poc 2>&1 | tee run.log

# Detach from screen: Ctrl-A then D
# Reattach later: screen -r arb
```

Alternative using nohup (simpler):
```bash
cd ~
mkdir -p logs
RUST_LOG=info nohup ./polymarket-arb-poc > run.log 2>&1 &
echo $! > pid.txt

# Check it's running
tail -f run.log

# Check status later
cat pid.txt | xargs ps -p
```

### Monitor During Run

```bash
# Watch trades in real-time
tail -f ~/logs/mock_trades.jsonl | python3 -m json.tool

# Count trades so far
wc -l ~/logs/mock_trades.jsonl

# Watch latest evaluations
tail -1 ~/logs/evaluations.jsonl | python3 -m json.tool

# Quick latency check
tail -100 ~/logs/latency_metrics.csv | awk -F, '{sum+=$4; n++} END {print "Avg decision_duration_us:", sum/n}'
```

---

## 8. After the Run — Collect Results

### On EC2: package logs

```bash
cd ~

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

# Edge-positive trade summary
python3 -c "
import json
trades = [json.loads(l) for l in open('logs/mock_trades.jsonl')]
if trades:
    edge_pos = [t for t in trades if t.get('edge_positive')]
    print(f'Total trades: {len(trades)}')
    print(f'Edge-positive trades: {len(edge_pos)}')
    avg_exec = sum(t['combined_exec'] for t in trades) / len(trades)
    print(f'Avg combined_exec: {avg_exec:.4f}')
    total_net = sum(t['expected_profit_net'] for t in trades)
    print(f'Total expected net PnL: {total_net:.2f}')
    settled = [t for t in trades if t.get('realized_profit') is not None]
    if settled:
        realized = sum(t['realized_profit'] for t in settled)
        print(f'Settled trades: {len(settled)}')
        print(f'Realized PnL: {realized:.2f}')
    by_sym = {}
    for t in trades:
        by_sym.setdefault(t['symbol'], []).append(t)
    for sym, ts in sorted(by_sym.items()):
        ep = [t for t in ts if t.get('edge_positive')]
        print(f'  {sym}: {len(ts)} trades ({len(ep)} edge+), avg combined_exec={sum(t[\"combined_exec\"] for t in ts)/len(ts):.4f}')
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

### Pull results to your local machine

```bash
# From your local machine:
scp -i your-key.pem ec2-user@<public-ip>:~/results-*.tar.gz .
```

---

## 9. Updating the Binary

When there are code changes, rebuild locally and push the new binary:

```bash
# On your local machine:
cd polymarket-arb-poc
git pull
cargo build --release   # or cross build for ARM

# Push updated binary (stop the running bot first via SSH)
scp -i your-key.pem target/release/polymarket-arb-poc ec2-user@<public-ip>:~/

# SSH in and restart
ssh -i your-key.pem ec2-user@<public-ip>
# Kill old process if running:
kill $(cat pid.txt) 2>/dev/null
RUST_LOG=info nohup ./polymarket-arb-poc > run.log 2>&1 &
echo $! > pid.txt
```

---

## 10. Share Results With Me

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
If uploading isn't convenient, run the summary scripts from Section 8 and paste the output. That gives me enough to start tuning.

---

## 11. What I'll Analyze

With your results, I'll compute:

1. **Opportunity frequency** — how many trades per hour, per symbol
2. **Combined exec distribution** — histogram of `up_exec + down_exec` values (after slippage)
3. **Edge-positive rate** — what fraction of trades have `expected_profit_net > 0`
4. **Latency budget** — is signal-to-decision within 40-80ms?
5. **Realized PnL** — actual profit using Polymarket resolution, fees, slippage, and simulated delay
6. **Threshold tuning** — should we tighten from 0.95 to 0.98? Or loosen?
7. **Time-of-day patterns** — are opportunities clustered around specific hours?
8. **Symbol comparison** — which assets have the best edge?
9. **Next steps** — whether to proceed to live trading, and with what parameters

---

## 12. Cost Estimate

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
# Check architecture matches
file ~/polymarket-arb-poc
# Should show: ELF 64-bit ... ARM aarch64 (for t4g)
# Or: ELF 64-bit ... x86-64 (for t3)

# If architecture mismatch: rebuild for the correct target
# Check if port/resource issues
lsof -i :443
# Check logs
tail -50 run.log
```

### "config.toml not found"
The binary looks for `config.toml` in the current working directory. Make sure you `cd ~` before running, and that `config.toml` is in `~/`.

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
