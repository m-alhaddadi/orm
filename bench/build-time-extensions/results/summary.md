# Build-time extension refactor: old versus current

| Runtime / backend / workload | Old µs | Current µs | Change | Paired 95% interval | Run changes |
|---|---:|---:|---:|---:|---|
| python/postgres/definition | 184.65 | 187.99 | +1.8% | [-0.0%, +2.0%] | +1.2%, +1.8%, +1.9% |
| python/postgres/construct | 11.64 | 11.69 | +0.4% | [+0.2%, +0.9%] | +0.4%, +0.4%, -0.8% |
| python/postgres/plan-native | 1.65 | 1.63 | -1.8% | [-1.6%, -0.2%] | -2.3%, +0.3%, -1.6% |
| python/postgres/construct+sql | 23.82 | 23.86 | +0.2% | [-0.4%, +0.5%] | +0.2%, -0.2%, +0.4% |
| python/postgres/read-1 | 498.61 | 498.78 | +0.0% | [-1.0%, +0.3%] | -0.4%, +0.0%, +0.3% |
| python/postgres/read-50 | 547.16 | 546.77 | -0.1% | [-0.8%, +0.5%] | -0.3%, -0.1%, -0.3% |
| python/postgres/read-1000 | 1450.82 | 1445.74 | -0.4% | [-1.2%, -0.3%] | -0.7%, -0.7%, -0.4% |
| python/postgres/projection | 533.50 | 533.46 | -0.0% | [-0.5%, +0.3%] | -0.2%, +0.1%, -0.0% |
| python/postgres/update | 148.09 | 148.61 | +0.4% | [-1.0%, +1.5%] | -0.2%, +0.5%, +0.8% |
| python/postgres/bulk-update-50 | 344.62 | 347.76 | +0.9% | [-0.5%, +2.5%] | +1.9%, +0.9%, +0.5% |
| python/postgres/insert | 142.51 | 142.40 | -0.1% | [-1.4%, +1.4%] | -0.1%, -0.2%, -0.1% |
| python/postgres/bulk-insert-50 | 472.45 | 471.57 | -0.2% | [-0.2%, +1.2%] | +0.0%, -0.2%, -0.8% |
| python/sqlite/definition | 187.19 | 187.43 | +0.1% | [-0.1%, +2.7%] | +1.3%, +0.2%, +0.1% |
| python/sqlite/construct | 11.50 | 11.56 | +0.5% | [+0.2%, +1.0%] | +0.2%, +0.8%, +1.0% |
| python/sqlite/plan-native | 1.65 | 1.63 | -1.0% | [-1.6%, -0.3%] | -2.4%, +1.6%, -1.0% |
| python/sqlite/construct+sql | 23.70 | 23.95 | +1.1% | [+0.2%, +1.1%] | +1.2%, +0.4%, +1.1% |
| python/sqlite/read-1 | 224.66 | 223.95 | -0.3% | [-0.2%, +1.1%] | +1.0%, -0.3%, +0.8% |
| python/sqlite/read-50 | 255.01 | 254.65 | -0.1% | [-0.2%, +0.5%] | +0.7%, -0.1%, +0.4% |
| python/sqlite/read-1000 | 1104.31 | 1112.14 | +0.7% | [-0.3%, +0.6%] | -0.1%, +0.7%, -0.2% |
| python/sqlite/projection | 252.25 | 249.79 | -1.0% | [-0.6%, +0.4%] | +0.1%, -1.0%, -0.2% |
| python/sqlite/update | 91.28 | 91.32 | +0.0% | [-0.3%, +0.9%] | +1.0%, +0.1%, -0.2% |
| python/sqlite/bulk-update-50 | 236.51 | 234.76 | -0.7% | [-1.4%, +0.4%] | -0.8%, +0.3%, -1.0% |
| python/sqlite/insert | 81.90 | 83.40 | +1.8% | [-0.2%, +2.4%] | +3.0%, -1.6%, +1.8% |
| python/sqlite/bulk-insert-50 | 209.97 | 212.13 | +1.0% | [+0.0%, +2.4%] | +0.4%, +1.0%, +1.4% |
| node/postgres/definition | 147.24 | 149.46 | +1.5% | [-0.4%, +3.0%] | +0.5%, +2.2%, +1.5% |
| node/postgres/construct | 1.62 | 1.40 | -13.4% | [-15.4%, -8.5%] | +1.3%, -16.5%, -13.4% |
| node/postgres/plan-native | 1.87 | 1.86 | -0.5% | [-0.9%, +0.8%] | +0.4%, +0.3%, -1.1% |
| node/postgres/construct+sql | 8.89 | 8.80 | -1.0% | [-7.4%, +3.7%] | -0.3%, -1.0%, -1.0% |
| node/postgres/read-1 | 391.65 | 391.96 | +0.1% | [-0.6%, +1.4%] | +0.1%, +1.2%, +0.3% |
| node/postgres/read-50 | 454.85 | 458.93 | +0.9% | [-0.1%, +1.5%] | +0.9%, +0.3%, +0.7% |
| node/postgres/read-1000 | 1517.29 | 1523.23 | +0.4% | [-0.3%, +1.7%] | +0.4%, +0.5%, +1.2% |
| node/postgres/projection | 487.29 | 489.69 | +0.5% | [-0.1%, +0.8%] | +0.5%, -0.2%, +0.3% |
| node/postgres/update | 111.46 | 111.07 | -0.4% | [-1.9%, +0.8%] | -0.2%, +0.2%, -0.8% |
| node/postgres/bulk-update-50 | 289.37 | 291.94 | +0.9% | [+0.3%, +1.9%] | +0.9%, +0.8%, +0.6% |
| node/postgres/insert | 112.25 | 112.21 | -0.0% | [-1.1%, +0.1%] | +0.0%, -0.9%, -0.4% |
| node/postgres/bulk-insert-50 | 451.65 | 455.83 | +0.9% | [-1.7%, +1.1%] | +2.3%, -0.4%, -0.8% |
| node/sqlite/definition | 148.40 | 149.12 | +0.5% | [-0.9%, +2.7%] | +0.3%, +1.0%, +0.6% |
| node/sqlite/construct | 1.58 | 1.39 | -11.7% | [-14.0%, -6.9%] | -13.1%, -1.2%, -11.7% |
| node/sqlite/plan-native | 1.84 | 1.84 | -0.2% | [-1.1%, +0.0%] | +0.1%, -0.2%, -0.9% |
| node/sqlite/construct+sql | 8.74 | 8.56 | -2.0% | [-9.5%, +5.7%] | -0.5%, -0.5%, -3.6% |
| node/sqlite/read-1 | 129.67 | 131.09 | +1.1% | [-0.6%, +3.1%] | +0.9%, +2.3%, +0.9% |
| node/sqlite/read-50 | 177.40 | 177.17 | -0.1% | [-0.9%, +2.1%] | -0.6%, +2.3%, -0.3% |
| node/sqlite/read-1000 | 1155.46 | 1144.45 | -1.0% | [-1.7%, +1.2%] | -2.0%, +0.3%, +0.2% |
| node/sqlite/projection | 162.75 | 166.50 | +2.3% | [+0.7%, +3.5%] | -0.2%, +4.0%, +2.3% |
| node/sqlite/update | 47.83 | 48.55 | +1.5% | [-1.5%, +2.0%] | +2.7%, -2.9%, +0.6% |
| node/sqlite/bulk-update-50 | 193.86 | 191.61 | -1.2% | [-2.5%, +1.1%] | -1.0%, -0.6%, -2.1% |
| node/sqlite/insert | 48.80 | 48.76 | -0.1% | [-4.0%, +1.6%] | -1.1%, +0.4%, -0.8% |
| node/sqlite/bulk-insert-50 | 221.59 | 221.80 | +0.1% | [-2.1%, +1.6%] | -0.1%, +0.1%, -0.6% |

Positive changes mean slower execution. Definition is setup cost and includes preparation to the same ready-to-query state in both versions.

Warm regressions above accepted cost (lower bound ≥ 1%): 0.

Warm cases with upper bound below 1%: 20/44.
Inconclusive against the 1% limit: 24.
The user accepts costs strictly below 1%. The gate requires an upper confidence bound below the limit. Intervals are exploratory and have no correction for multiple comparisons.
