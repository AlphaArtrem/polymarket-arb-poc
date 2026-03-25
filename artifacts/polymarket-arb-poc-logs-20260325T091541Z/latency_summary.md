# Polymarket Arb POC Run Summary

Source archive: `polymarket-arb-poc-logs-20260325T091541Z.tar.gz`

## Run Window

- Start (UTC): 2026-03-25 09:09:51.492
- End (UTC): 2026-03-25 09:15:28.521
- Duration: 337.029 seconds
- Evaluations: 70,573
- Triggered trades: 0
- Positive edges: 0

## Overall Latency

| Metric | Mean | P50 | P90 | P95 | P99 | Max |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `binance_to_decision_us` | 0.949 | 0 | 0 | 0 | 0 | 1000 |
| `poly_to_decision_us` | 286861.491 | 15000 | 1020000 | 1726000 | 3617000 | 5296000 |
| `decision_duration_us` | 0.995 | 0 | 3 | 4 | 5 | 103 |
| `simulated_order_delay_ms` | 10.000 | 10 | 10 | 10 | 10 | 10 |

## By Symbol

| Symbol | Evaluations | `binance_to_decision_us` Mean | `binance_to_decision_us` P95 | `poly_to_decision_us` Mean | `poly_to_decision_us` P95 | `decision_duration_us` Mean | `decision_duration_us` P95 | `combined_ask` Min | `combined_ask` P05 | `combined_ask` P50 | Trades | Positive Edges |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| `btc` | 31697 | 0.915 | 0 | 133937.029 | 889000 | 0.882 | 4 | 1.01 | 1.01 | 1.01 | 0 | 0 |
| `eth` | 22737 | 1.407 | 0 | 375804.812 | 2184000 | 1.161 | 4 | 1.01 | 1.01 | 1.01 | 0 | 0 |
| `sol` | 10268 | 0.292 | 0 | 481127.678 | 2096000 | 1.000 | 4 | 1.00 | 1.01 | 1.01 | 0 | 0 |
| `xrp` | 5871 | 0.511 | 0 | 428271.334 | 2290000 | 0.957 | 4 | 1.01 | 1.01 | 1.01 | 0 | 0 |

## Notes

- Decision computation inside the strategy loop was effectively negligible in this sample.
- The dominant freshness lag was `poly_to_decision_us`, not local decision time.
- `combined_ask` was never below `1.00` and was almost always `1.01`, so the configured `0.95` threshold never came close to triggering a paper trade.
