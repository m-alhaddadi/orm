# Build-time extension refactor: handwritten versus extension

| Runtime / backend / workload | Handwritten µs | Extension µs | Paired median change | Paired 95% interval | Run changes |
|---|---:|---:|---:|---:|---|
| python/postgres/definition | 298.96 | 323.20 | -1.0% | [-6.4%, +12.9%] | +8.1%, +2.6%, +7.7% |
| python/postgres/construct | 3.43 | 3.26 | -0.3% | [-5.9%, +5.6%] | -4.7%, -1.9%, -18.9% |
| python/postgres/plan-native | 1.23 | 1.20 | -1.4% | [-6.1%, +3.5%] | -3.4%, +0.5%, -4.4% |
| python/postgres/construct+sql | 16.22 | 17.91 | -2.7% | [-14.8%, +16.9%] | +16.4%, +3.2%, +8.3% |
| python/postgres/read-1 | 1043.18 | 1029.61 | -3.5% | [-8.0%, +8.6%] | -5.4%, -7.3%, +12.6% |
| python/postgres/read-50 | 1230.37 | 1026.31 | -4.2% | [-8.7%, +0.2%] | -17.2%, -0.5%, -4.2% |
| python/postgres/read-1000 | 3151.05 | 2891.84 | -5.5% | [-11.2%, +5.6%] | -9.1%, -10.0%, -0.5% |
| python/postgres/projection | 995.46 | 976.91 | +0.8% | [-4.6%, +7.1%] | -1.9%, -2.2%, +1.7% |
| python/postgres/update | 663.86 | 670.73 | -3.8% | [-6.0%, +3.9%] | -3.0%, +1.0%, +0.4% |
| python/postgres/bulk-update-50 | 905.04 | 940.46 | -0.4% | [-1.7%, +5.6%] | -0.5%, +3.9%, -1.3% |
| python/postgres/insert | 633.24 | 623.25 | -0.7% | [-3.2%, +3.6%] | -1.6%, +19.4%, -5.6% |
| python/postgres/bulk-insert-50 | 1098.36 | 1133.94 | +2.8% | [-0.3%, +4.8%] | +3.0%, +3.2%, -4.7% |
| python/sqlite/definition | 360.95 | 375.48 | +2.3% | [-4.3%, +12.0%] | +4.0%, -0.0%, -0.5% |
| python/sqlite/construct | 2.41 | 3.09 | -0.3% | [-14.0%, +0.8%] | -10.1%, +35.0%, +0.6% |
| python/sqlite/plan-native | 1.49 | 1.25 | -1.7% | [-14.9%, +2.5%] | -1.7%, -16.1%, -5.4% |
| python/sqlite/construct+sql | 11.27 | 14.08 | +3.9% | [-2.6%, +17.1%] | +6.4%, +45.0%, +4.1% |
| python/sqlite/read-1 | 443.48 | 453.30 | -0.6% | [-4.9%, +5.6%] | +2.2%, +25.8%, -0.3% |
| python/sqlite/read-50 | 776.38 | 530.90 | +6.1% | [-5.1%, +15.1%] | -32.4%, -0.9%, -1.7% |
| python/sqlite/read-1000 | 2055.42 | 2129.57 | +1.7% | [-5.5%, +9.6%] | +3.6%, -10.1%, +3.6% |
| python/sqlite/projection | 405.25 | 383.82 | -4.9% | [-11.8%, -0.0%] | -3.9%, +6.0%, -5.3% |
| python/sqlite/update | 132.03 | 133.40 | -0.4% | [-2.9%, +5.1%] | -1.6%, -8.6%, +1.0% |
| python/sqlite/bulk-update-50 | 324.76 | 323.00 | +1.6% | [-4.7%, +7.9%] | -0.5%, -10.9%, -1.9% |
| python/sqlite/insert | 123.38 | 127.57 | +1.3% | [-2.4%, +5.7%] | -2.0%, +19.1%, +3.4% |
| python/sqlite/bulk-insert-50 | 356.73 | 373.91 | +1.4% | [-4.9%, +7.1%] | +1.2%, +14.1%, +4.8% |
| node/postgres/definition | 255.42 | 241.71 | -1.0% | [-12.0%, +2.7%] | -5.0%, -5.4%, -1.5% |
| node/postgres/construct | 1.98 | 2.01 | -3.4% | [-10.1%, +5.8%] | +1.7%, -12.3%, -1.9% |
| node/postgres/plan-native | 1.79 | 2.26 | +1.1% | [-2.6%, +4.5%] | -2.3%, +26.0%, +1.0% |
| node/postgres/construct+sql | 7.06 | 8.80 | -8.1% | [-11.2%, +4.3%] | -10.9%, +24.6%, -4.4% |
| node/postgres/read-1 | 690.43 | 695.96 | +0.7% | [-2.2%, +3.6%] | -1.3%, +0.8%, +3.5% |
| node/postgres/read-50 | 843.74 | 845.75 | -1.2% | [-2.1%, +1.7%] | -5.7%, -0.8%, +0.2% |
| node/postgres/read-1000 | 2307.10 | 2259.99 | +1.6% | [-2.7%, +3.1%] | +2.3%, +1.4%, -2.0% |
| node/postgres/projection | 688.21 | 686.67 | -1.6% | [-2.6%, +1.2%] | +1.6%, -4.8%, -0.2% |
| node/postgres/update | 580.57 | 568.78 | -1.0% | [-2.6%, +3.8%] | +0.2%, +4.6%, -2.0% |
| node/postgres/bulk-update-50 | 784.03 | 815.02 | +1.7% | [-1.2%, +3.8%] | +4.8%, +1.5%, -1.6% |
| node/postgres/insert | 557.46 | 550.34 | -2.5% | [-5.1%, +0.7%] | +0.3%, +0.0%, -1.3% |
| node/postgres/bulk-insert-50 | 988.02 | 997.33 | +0.6% | [-1.7%, +3.8%] | +3.0%, +0.9%, +2.1% |
| node/sqlite/definition | 209.88 | 196.62 | -3.0% | [-8.0%, +5.3%] | +1.7%, -0.2%, -6.3% |
| node/sqlite/construct | 1.81 | 1.79 | -1.4% | [-14.7%, +8.5%] | +2.6%, -1.1%, -9.5% |
| node/sqlite/plan-native | 2.14 | 2.70 | +0.5% | [-3.8%, +4.1%] | -4.1%, -3.5%, +25.8% |
| node/sqlite/construct+sql | 7.26 | 9.43 | +10.2% | [+0.3%, +25.2%] | +12.6%, +18.4%, +29.9% |
| node/sqlite/read-1 | 156.09 | 167.48 | +8.2% | [+0.2%, +10.9%] | +3.8%, -0.1%, +7.3% |
| node/sqlite/read-50 | 258.78 | 281.58 | +2.6% | [-0.7%, +11.2%] | +4.8%, +1.4%, +10.4% |
| node/sqlite/read-1000 | 1824.73 | 1688.21 | -3.3% | [-9.1%, +5.1%] | -2.4%, -10.8%, +2.1% |
| node/sqlite/projection | 258.88 | 266.35 | +2.3% | [-3.2%, +7.1%] | -1.3%, +3.6%, -1.3% |
| node/sqlite/update | 62.10 | 55.82 | -2.7% | [-10.3%, +2.5%] | -4.9%, -10.1%, -5.4% |
| node/sqlite/bulk-update-50 | 283.52 | 294.87 | -0.7% | [-6.6%, +6.9%] | +8.0%, +4.0%, -0.3% |
| node/sqlite/insert | 79.13 | 79.46 | -3.2% | [-6.9%, +15.5%] | +0.4%, -3.1%, +0.1% |
| node/sqlite/bulk-insert-50 | 331.55 | 333.23 | +5.1% | [-3.7%, +10.4%] | -6.7%, +1.7%, +1.3% |

Positive changes mean slower execution. Definition is setup cost and includes preparation to the same ready-to-query state in both versions.

Warm regressions above accepted cost (lower bound ≥ 1%): 0.

Warm cases with upper bound below 1%: 4/44.
Inconclusive against the 1% limit: 40.
The user accepts costs strictly below 1%. The gate requires an upper confidence bound below the limit. Intervals are exploratory and have no correction for multiple comparisons.
