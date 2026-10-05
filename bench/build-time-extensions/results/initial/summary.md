# Build-time extension refactor: old versus current

| Runtime / backend / workload | Old µs | Current µs | Change | Paired 95% interval | Run changes |
|---|---:|---:|---:|---:|---|
| python/postgres/definition | 181.60 | 183.71 | +1.2% | [+0.7%, +1.3%] | +1.2%, +1.0%, +1.2% |
| python/postgres/construct | 11.68 | 11.74 | +0.5% | [+0.4%, +1.4%] | +0.8%, +1.2%, +0.5% |
| python/postgres/plan-native | 1.72 | 1.72 | +0.4% | [-1.5%, +0.8%] | -0.7%, +0.9%, -1.4% |
| python/postgres/construct+sql | 24.29 | 24.42 | +0.6% | [+0.4%, +1.4%] | +0.9%, +0.8%, +0.6% |
| python/postgres/read-1 | 355.58 | 354.31 | -0.4% | [-1.1%, +0.4%] | +0.5%, -0.3%, -0.4% |
| python/postgres/read-50 | 410.76 | 408.45 | -0.6% | [-1.8%, +0.3%] | -0.6%, +0.1%, -1.4% |
| python/postgres/read-1000 | 1327.17 | 1317.02 | -0.8% | [-1.3%, +0.4%] | -0.8%, +0.3%, +0.1% |
| python/postgres/projection | 454.56 | 453.99 | -0.1% | [-0.8%, +0.2%] | -0.1%, +0.0%, -1.5% |
| python/postgres/update | 186.60 | 184.72 | -1.0% | [-1.6%, +0.4%] | -1.2%, +0.1%, -0.9% |
| python/postgres/bulk-update-50 | 463.70 | 468.97 | +1.1% | [-0.6%, +4.7%] | +1.1%, +4.9%, +0.8% |
| python/postgres/insert | 180.69 | 180.21 | -0.3% | [-1.6%, +0.8%] | -0.6%, -0.5%, +1.4% |
| python/postgres/bulk-insert-50 | 466.97 | 466.52 | -0.1% | [-2.1%, +0.7%] | -0.8%, +0.3%, -0.7% |
| python/sqlite/definition | 181.45 | 182.77 | +0.7% | [+0.4%, +0.9%] | +0.6%, +1.0%, +0.7% |
| python/sqlite/construct | 11.63 | 11.68 | +0.4% | [-0.6%, +0.9%] | +0.4%, +0.2%, +0.6% |
| python/sqlite/plan-native | 1.69 | 1.71 | +1.1% | [-0.3%, +1.8%] | +1.0%, -0.2%, +1.2% |
| python/sqlite/construct+sql | 24.22 | 24.35 | +0.5% | [+0.1%, +1.0%] | +0.5%, +0.3%, +1.1% |
| python/sqlite/read-1 | 233.00 | 229.58 | -1.5% | [-1.8%, +0.5%] | -1.5%, +0.3%, -0.7% |
| python/sqlite/read-50 | 263.63 | 260.51 | -1.2% | [-0.8%, +0.1%] | -1.2%, -0.3%, +0.6% |
| python/sqlite/read-1000 | 1113.27 | 1128.66 | +1.4% | [+0.7%, +2.3%] | -0.9%, +1.3%, +2.3% |
| python/sqlite/projection | 258.66 | 257.56 | -0.4% | [-0.8%, +0.1%] | -0.4%, -0.5%, -0.2% |
| python/sqlite/update | 97.31 | 97.15 | -0.2% | [-0.9%, +1.0%] | -0.8%, -0.3%, +0.4% |
| python/sqlite/bulk-update-50 | 260.20 | 260.05 | -0.1% | [-0.5%, +1.3%] | +1.4%, -1.6%, -0.1% |
| python/sqlite/insert | 89.37 | 89.38 | +0.0% | [-0.8%, +0.9%] | +0.7%, -0.4%, +0.0% |
| python/sqlite/bulk-insert-50 | 246.10 | 246.52 | +0.2% | [-1.6%, +1.1%] | -0.2%, +0.2%, -0.1% |
| node/postgres/definition | 139.40 | 141.71 | +1.7% | [+0.5%, +1.7%] | +0.5%, +1.7%, +1.2% |
| node/postgres/construct | 1.59 | 1.65 | +3.7% | [+2.6%, +4.3%] | +4.2%, +3.4%, +2.9% |
| node/postgres/plan-native | 1.85 | 1.85 | +0.5% | [+0.0%, +1.5%] | +0.0%, +0.8%, +0.6% |
| node/postgres/construct+sql | 9.09 | 9.40 | +3.4% | [+2.2%, +5.6%] | +3.4%, +3.7%, +5.3% |
| node/postgres/read-1 | 351.04 | 349.86 | -0.3% | [-0.6%, +0.9%] | -0.3%, -0.1%, +0.3% |
| node/postgres/read-50 | 419.05 | 420.38 | +0.3% | [-0.7%, +0.7%] | +0.3%, -0.8%, +0.4% |
| node/postgres/read-1000 | 1479.87 | 1479.34 | -0.0% | [-0.3%, +0.4%] | -0.0%, -0.9%, +0.5% |
| node/postgres/projection | 397.19 | 396.71 | -0.1% | [-0.4%, +0.9%] | +0.4%, -0.1%, -0.1% |
| node/postgres/update | 109.47 | 108.21 | -1.1% | [-0.9%, +0.9%] | -1.2%, -0.2%, +0.0% |
| node/postgres/bulk-update-50 | 281.01 | 297.63 | +5.9% | [-0.7%, +4.4%] | +1.7%, -0.3%, +5.9% |
| node/postgres/insert | 112.38 | 112.38 | -0.0% | [-1.2%, +0.5%] | -0.0%, -0.3%, +0.0% |
| node/postgres/bulk-insert-50 | 384.77 | 387.33 | +0.7% | [-0.5%, +2.5%] | +1.3%, +0.7%, -3.0% |
| node/sqlite/definition | 140.46 | 141.04 | +0.4% | [+0.2%, +0.7%] | +0.3%, +0.7%, +0.5% |
| node/sqlite/construct | 1.60 | 1.62 | +1.8% | [+1.4%, +3.7%] | +2.4%, +1.5%, +1.8% |
| node/sqlite/plan-native | 1.87 | 1.87 | -0.0% | [-2.7%, +0.1%] | -1.5%, -2.4%, +2.1% |
| node/sqlite/construct+sql | 8.91 | 9.23 | +3.6% | [+3.5%, +5.2%] | +2.9%, +4.3%, +4.0% |
| node/sqlite/read-1 | 128.52 | 129.64 | +0.9% | [-1.9%, +1.9%] | -3.3%, +0.6%, +2.2% |
| node/sqlite/read-50 | 176.99 | 176.55 | -0.2% | [-1.0%, +1.0%] | -0.2%, +0.1%, +0.4% |
| node/sqlite/read-1000 | 1149.54 | 1135.63 | -1.2% | [-1.4%, +0.5%] | -1.3%, -0.2%, -0.5% |
| node/sqlite/projection | 161.15 | 162.69 | +1.0% | [-0.3%, +1.9%] | +1.1%, +0.5%, +0.3% |
| node/sqlite/update | 48.65 | 46.90 | -3.6% | [-4.5%, -0.3%] | -3.6%, -2.3%, -0.6% |
| node/sqlite/bulk-update-50 | 197.85 | 197.34 | -0.3% | [-1.3%, +1.1%] | -0.3%, -0.3%, -0.3% |
| node/sqlite/insert | 49.88 | 49.64 | -0.5% | [-1.7%, +3.2%] | -0.7%, +0.2%, +0.4% |
| node/sqlite/bulk-insert-50 | 225.01 | 226.06 | +0.5% | [-0.3%, +2.0%] | +1.5%, -0.4%, +0.5% |

Positive changes mean slower execution. Definition is setup cost and includes preparation to the same ready-to-query state in both versions.

Detected warm timing regressions (positive lower bound): 9.
- python/postgres/construct: [+0.4%, +1.4%]
- python/postgres/construct+sql: [+0.4%, +1.4%]
- python/sqlite/construct+sql: [+0.1%, +1.0%]
- python/sqlite/read-1000: [+0.7%, +2.3%]
- node/postgres/construct: [+2.6%, +4.3%]
- node/postgres/plan-native: [+0.0%, +1.5%]
- node/postgres/construct+sql: [+2.2%, +5.6%]
- node/sqlite/construct: [+1.4%, +3.7%]
- node/sqlite/construct+sql: [+3.5%, +5.2%]

Warm cases whose entire paired interval lies within ±3%: 36/44.
The ±3% count describes measurement precision; it does not waive smaller detected regressions.
Intervals spanning zero are inconclusive, not proof of equivalence. Intervals are exploratory and have no correction for multiple comparisons.
