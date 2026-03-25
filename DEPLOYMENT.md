# VPS Deployment and Artifact Collection

Use this runbook only for live `paper` sessions where VPS network location can materially change results. Keep code changes, tests, and analysis work on the local machine.

## Purpose

- Prefer local development and verification.
- Use a VPS only for bounded live collection runs against Polymarket and Binance.
- Always copy the full session directory back to local `artifacts/paper/` before deleting the instance.

## Prerequisites

- Local repo is clean and ready to deploy at `/Users/alphaartrem/Desktop/workspace/polymarket-arb-poc`.
- SSH key exists locally at `/Users/alphaartrem/ec2_poly.pem`.
- Target host is reachable:

```bash
ssh -i ~/ec2_poly.pem ec2-user@<host>
```

- Target instance is Amazon Linux 2023 on `aarch64` (eu-west-1, Dublin).
- Instance type: `t4g.medium` (2 vCPU ARM Graviton, 4 GB RAM, ~$0.81/day).
- Security group: outbound all, inbound SSH only (port 22, your IP).

## Preferred Path

Use this when you have a working Linux ARM release binary locally.

### 1. Build the binary locally

```bash
cd /Users/alphaartrem/Desktop/workspace/polymarket-arb-poc
cargo build --release --target aarch64-unknown-linux-gnu
```

If local cross-compilation does not work (OpenSSL / native-tls linker issues on macOS), skip to the fallback path.

### 2. Upload only the runtime payload

```bash
rsync -az -e 'ssh -i /Users/alphaartrem/ec2_poly.pem' \
  target/aarch64-unknown-linux-gnu/release/polymarket-arb-poc \
  config.toml \
  ec2-user@<host>:/home/ec2-user/arb-upload/
```

### 3. Prepare the VPS

```bash
ssh -i /Users/alphaartrem/ec2_poly.pem ec2-user@<host> 'bash -s' <<'EOF'
set -euo pipefail
mkdir -p /home/ec2-user/arb/logs
cp /home/ec2-user/arb-upload/polymarket-arb-poc /home/ec2-user/arb/polymarket-arb-poc
chmod +x /home/ec2-user/arb/polymarket-arb-poc
cp /home/ec2-user/arb-upload/config.toml /home/ec2-user/arb/config.toml
EOF
```

### 4. Verify NTP sync

```bash
ssh -i /Users/alphaartrem/ec2_poly.pem ec2-user@<host> 'chronyc sources'
# Expect: ^* 169.254.169.123 ... (sub-ms sync)
```

### 5. Run the pre-flight latency check

```bash
ssh -i /Users/alphaartrem/ec2_poly.pem ec2-user@<host> 'bash -s' <<'EOF'
set -euo pipefail
echo "=== Pre-flight latency ===" > /home/ec2-user/arb/pre_flight.txt
ping -c 10 stream.binance.com >> /home/ec2-user/arb/pre_flight.txt 2>&1
ping -c 10 ws-subscriptions-clob.polymarket.com >> /home/ec2-user/arb/pre_flight.txt 2>&1
cat /home/ec2-user/arb/pre_flight.txt
EOF
```

### 6. Run the bounded paper session

Replace `<session-id>` with a descriptive tag like `dir-v1-20260325T160000Z` and `<bounded-seconds>` with the run duration (e.g., `86400` for 24 hours, `3600` for 1 hour).

```bash
ssh -i /Users/alphaartrem/ec2_poly.pem ec2-user@<host> 'bash -s' <<'EOF'
set -euo pipefail
cd /home/ec2-user/arb
mkdir -p logs

# Start the session in the background
RUST_LOG=info nohup ./polymarket-arb-poc > run.log 2>&1 &
echo $! > pid.txt
echo "Started PID $(cat pid.txt)"

# Verify it is running
sleep 3
if ps -p $(cat pid.txt) > /dev/null 2>&1; then
  echo "Process is running"
  tail -5 run.log
else
  echo "ERROR: Process failed to start"
  tail -20 run.log
fi
EOF
```

### 7. Monitor during the run (optional)

```bash
# Watch trades in real-time
ssh -i /Users/alphaartrem/ec2_poly.pem ec2-user@<host> 'tail -f /home/ec2-user/arb/logs/mock_trades.jsonl'

# Count trades so far
ssh -i /Users/alphaartrem/ec2_poly.pem ec2-user@<host> 'wc -l /home/ec2-user/arb/logs/mock_trades.jsonl /home/ec2-user/arb/logs/signals.jsonl 2>/dev/null'

# Check if process is still alive
ssh -i /Users/alphaartrem/ec2_poly.pem ec2-user@<host> 'ps -p $(cat /home/ec2-user/arb/pid.txt) && echo RUNNING || echo STOPPED'
```

### 8. Copy artifacts back locally

```bash
SESSION_ID="<session-id>"
mkdir -p artifacts/paper/${SESSION_ID}

rsync -az -e 'ssh -i /Users/alphaartrem/ec2_poly.pem -o BatchMode=yes' \
  ec2-user@<host>:/home/ec2-user/arb/logs/ \
  artifacts/paper/${SESSION_ID}/logs/

rsync -az -e 'ssh -i /Users/alphaartrem/ec2_poly.pem -o BatchMode=yes' \
  ec2-user@<host>:/home/ec2-user/arb/run.log \
  ec2-user@<host>:/home/ec2-user/arb/pre_flight.txt \
  ec2-user@<host>:/home/ec2-user/arb/config.toml \
  artifacts/paper/${SESSION_ID}/
```

### 9. Verify local artifacts before deleting the instance

```bash
ls -1 artifacts/paper/${SESSION_ID}/logs/
```

Expected files:

- `binance_ticks.jsonl`
- `polymarket_snapshots.jsonl`
- `mock_trades.jsonl`
- `signals.jsonl`
- `evaluations.jsonl`
- `latency_metrics.csv`

And in the session root:

- `run.log`
- `pre_flight.txt`
- `config.toml`

After local verification, terminate the instance.

---

## Fallback Path (Build on VPS)

Use this when local cross-compilation to `aarch64-unknown-linux-gnu` is blocked (e.g., OpenSSL / native-tls linker issues on macOS).

### 1. Sync the repo to the VPS

```bash
cd /Users/alphaartrem/Desktop/workspace/polymarket-arb-poc

rsync -az --delete \
  --exclude '.git' \
  --exclude 'target' \
  --exclude 'artifacts' \
  --exclude '*.DS_Store' \
  -e 'ssh -i /Users/alphaartrem/ec2_poly.pem -o BatchMode=yes' \
  ./ ec2-user@<host>:/home/ec2-user/arb-src/
```

### 2. Install build prerequisites and Rust on the VPS

Do not install `curl` with `dnf` on Amazon Linux 2023 — the instance has `curl-minimal` and full `curl` causes package conflicts.

```bash
ssh -i /Users/alphaartrem/ec2_poly.pem ec2-user@<host> 'bash -s' <<'EOF'
set -euo pipefail
sudo dnf install -y gcc gcc-c++ make perl-core openssl-devel pkgconf-pkg-config
if test ! -x "$HOME/.cargo/bin/cargo"; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
EOF
```

### 3. Build the release binary on the VPS

```bash
ssh -i /Users/alphaartrem/ec2_poly.pem ec2-user@<host> 'bash -s' <<'EOF'
set -euo pipefail
source "$HOME/.cargo/env"
cd /home/ec2-user/arb-src
cargo build --release
mkdir -p /home/ec2-user/arb/logs
cp target/release/polymarket-arb-poc /home/ec2-user/arb/polymarket-arb-poc
cp config.toml /home/ec2-user/arb/config.toml
EOF
```

### 4. Verify NTP, run pre-flight, start session

Follow steps 4 through 9 from the preferred path above. All VPS paths reference `/home/ec2-user/arb/` the same way.

---

## Quick Reference: One-Shot 24-Hour Run

If you just want to run the full flow end-to-end with minimal interaction:

```bash
# Variables — set these once
HOST="ec2-user@<host>"
KEY="/Users/alphaartrem/ec2_poly.pem"
SESSION="dir-v1-$(date -u +%Y%m%dT%H%M%SZ)"
REPO="/Users/alphaartrem/Desktop/workspace/polymarket-arb-poc"

# 1. Sync source (fallback path)
rsync -az --delete --exclude '.git' --exclude 'target' --exclude 'artifacts' --exclude '*.DS_Store' \
  -e "ssh -i $KEY -o BatchMode=yes" $REPO/ $HOST:/home/ec2-user/arb-src/

# 2. Build on VPS (skip if you already have the binary there)
ssh -i $KEY $HOST 'bash -s' <<'BUILDEOF'
set -euo pipefail
sudo dnf install -y gcc gcc-c++ make perl-core openssl-devel pkgconf-pkg-config 2>/dev/null || true
test -x "$HOME/.cargo/bin/cargo" || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
source "$HOME/.cargo/env"
cd /home/ec2-user/arb-src
cargo build --release
mkdir -p /home/ec2-user/arb/logs
cp target/release/polymarket-arb-poc /home/ec2-user/arb/polymarket-arb-poc
cp config.toml /home/ec2-user/arb/config.toml
BUILDEOF

# 3. Run pre-flight + start session
ssh -i $KEY $HOST 'bash -s' <<'RUNEOF'
set -euo pipefail
cd /home/ec2-user/arb
echo "=== Pre-flight ===" > pre_flight.txt
ping -c 10 stream.binance.com >> pre_flight.txt 2>&1
ping -c 10 ws-subscriptions-clob.polymarket.com >> pre_flight.txt 2>&1
RUST_LOG=info nohup ./polymarket-arb-poc > run.log 2>&1 &
echo $! > pid.txt
sleep 3
ps -p $(cat pid.txt) && echo "Session started" || echo "FAILED"
tail -5 run.log
RUNEOF

echo "Session $SESSION running. Collect artifacts after completion."

# 4. After the run — collect artifacts
mkdir -p artifacts/paper/$SESSION
rsync -az -e "ssh -i $KEY -o BatchMode=yes" \
  $HOST:/home/ec2-user/arb/logs/ artifacts/paper/$SESSION/logs/
rsync -az -e "ssh -i $KEY -o BatchMode=yes" \
  $HOST:/home/ec2-user/arb/run.log $HOST:/home/ec2-user/arb/pre_flight.txt $HOST:/home/ec2-user/arb/config.toml \
  artifacts/paper/$SESSION/

# 5. Verify
ls -1 artifacts/paper/$SESSION/logs/
echo "Done. Terminate instance when verified."
```

---

## Updating the Binary for New Runs

When code changes are pushed to master:

```bash
# Sync updated source
rsync -az --delete --exclude '.git' --exclude 'target' --exclude 'artifacts' --exclude '*.DS_Store' \
  -e "ssh -i $KEY -o BatchMode=yes" $REPO/ $HOST:/home/ec2-user/arb-src/

# Rebuild on VPS
ssh -i $KEY $HOST 'bash -s' <<'EOF'
set -euo pipefail
source "$HOME/.cargo/env"
cd /home/ec2-user/arb-src
cargo build --release
# Kill old process
kill $(cat /home/ec2-user/arb/pid.txt) 2>/dev/null || true
sleep 2
cp target/release/polymarket-arb-poc /home/ec2-user/arb/polymarket-arb-poc
cp config.toml /home/ec2-user/arb/config.toml
cd /home/ec2-user/arb
rm -rf logs/*
RUST_LOG=info nohup ./polymarket-arb-poc > run.log 2>&1 &
echo $! > pid.txt
sleep 3
ps -p $(cat pid.txt) && echo "Restarted" || echo "FAILED"
EOF
```

---

## Troubleshooting

### Binary fails to start
```bash
ssh -i $KEY $HOST 'file /home/ec2-user/arb/polymarket-arb-poc'
# Should show: ELF 64-bit ... ARM aarch64
ssh -i $KEY $HOST 'tail -30 /home/ec2-user/arb/run.log'
```

### "config.toml not found"
The binary looks for `config.toml` in the current working directory. The run commands `cd /home/ec2-user/arb` before launching.

### No Polymarket data
Markets rotate every 15 min. The discovery task needs ~2 min to fetch the first market. Check `run.log` for "Fetching Gamma API" and "Discovered N markets" messages.

### No trades triggered
Check `evaluations.jsonl` — if `price_vs_open_bps` never exceeds the threshold (30 bps), the market was flat during the run. Try a longer session or lower the threshold.

### Connection drops
The bot auto-reconnects with exponential backoff. Check `run.log` for "Reconnecting" messages.

### curl package conflict on Amazon Linux 2023
Do not run `sudo dnf install curl` — it conflicts with the pre-installed `curl-minimal`. The existing `curl-minimal` is sufficient for rustup installation.

---

## Notes

- A t4g.medium can run this flow. The first remote build takes ~3-5 minutes; subsequent builds are faster.
- If local cross-compilation is fixed later, prefer the precompiled upload path (preferred path) and keep the VPS as a pure collector.
- Cost: ~$0.84 for a 24-hour run on t4g.medium. Terminate after collecting artifacts.
