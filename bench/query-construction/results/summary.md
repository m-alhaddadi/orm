The direct operation decoder was rejected for an error compatibility change, regardless of timing.

| Runtime / backend / workload | Before µs | After µs | Change | Paired 95% interval | Three run changes |
|---|---:|---:|---:|---:|---|
| python/postgres/read-1 | 278.50 | 279.71 | +0.4% | [-0.6%, +0.8%] | +0.2%, -0.0%, +0.4% |
| python/postgres/read-50 | 315.85 | 316.20 | +0.1% | [-1.2%, +0.9%] | +0.4%, -0.0%, +0.1% |
| python/postgres/read-1000 | 966.01 | 964.25 | -0.2% | [-0.8%, +1.2%] | -2.0%, +0.9%, -0.2% |
| python/postgres/projection | 298.06 | 297.63 | -0.1% | [-0.6%, +0.3%] | -0.1%, -0.4%, -0.1% |
| python/postgres/nested-4 | 483.16 | 485.81 | +0.5% | [-0.4%, +0.5%] | +0.1%, -0.3%, +0.5% |
| python/postgres/nested-16 | 1194.49 | 1190.15 | -0.4% | [-0.9%, +0.2%] | -0.4%, -0.4%, -0.3% |
| python/postgres/shared | 443.79 | 441.96 | -0.4% | [-0.6%, +0.1%] | -0.5%, -0.4%, -0.6% |
| python/postgres/recursive | 750.83 | 753.52 | +0.4% | [-0.3%, +0.5%] | -0.0%, +0.4%, -0.2% |
| python/postgres/window-3 | 329.15 | 329.26 | +0.0% | [-0.8%, +0.1%] | +0.0%, -0.3%, -0.2% |
| python/postgres/window-16 | 448.76 | 448.44 | -0.1% | [-0.7%, +0.3%] | -0.1%, -0.7%, -0.4% |
| python/postgres/window-64 | 873.06 | 869.20 | -0.4% | [-0.7%, -0.0%] | -0.0%, -0.4%, -0.5% |
| python/sqlite/read-1 | 224.05 | 222.76 | -0.6% | [-1.2%, -0.2%] | -0.3%, -0.6%, -0.6% |
| python/sqlite/read-50 | 254.35 | 254.10 | -0.1% | [-1.1%, +0.1%] | -0.3%, -0.1%, -0.7% |
| python/sqlite/read-1000 | 941.28 | 939.76 | -0.2% | [-0.4%, +0.3%] | -0.2%, -0.3%, -0.3% |
| python/sqlite/projection | 222.74 | 222.72 | -0.0% | [-0.5%, +0.3%] | -0.0%, -0.3%, +0.1% |
| python/sqlite/nested-4 | 312.36 | 311.48 | -0.3% | [-0.6%, +0.3%] | -0.3%, -0.1%, -0.4% |
| python/sqlite/nested-16 | 567.05 | 575.88 | +1.6% | [-0.1%, +1.6%] | +1.6%, +0.1%, +1.7% |
| python/sqlite/shared | 281.40 | 281.00 | -0.1% | [-1.0%, -0.3%] | -1.1%, -0.7%, +0.2% |
| python/sqlite/recursive | 618.85 | 616.18 | -0.4% | [-0.1%, +0.3%] | -0.9%, +0.3%, +0.1% |
| node/postgres/read-1 | 168.20 | 167.15 | -0.6% | [-1.3%, +1.5%] | +0.5%, +0.7%, -0.6% |
| node/postgres/read-50 | 222.40 | 221.37 | -0.5% | [-1.0%, +1.3%] | +0.1%, +0.2%, -0.5% |
| node/postgres/read-1000 | 1184.96 | 1196.49 | +1.0% | [+0.7%, +1.6%] | +0.4%, +4.2%, +1.0% |
| node/postgres/projection | 176.40 | 176.55 | +0.1% | [-1.0%, +0.3%] | +0.1%, +0.2%, -0.4% |
| node/postgres/nested-4 | 200.83 | 201.15 | +0.2% | [-1.1%, +0.2%] | +0.2%, -0.2%, -0.3% |
| node/postgres/nested-16 | 303.43 | 297.31 | -2.0% | [-2.0%, -1.1%] | -2.0%, +0.4%, -1.5% |
| node/postgres/shared | 213.93 | 212.13 | -0.8% | [-2.1%, -0.1%] | -0.7%, -0.2%, -0.8% |
| node/postgres/recursive | 605.80 | 602.92 | -0.5% | [-0.9%, +0.2%] | -2.5%, +0.5%, -0.5% |
| node/postgres/window-3 | 165.05 | 163.16 | -1.1% | [-1.2%, +0.1%] | -0.6%, +0.2%, -1.1% |
| node/postgres/window-16 | 231.32 | 230.05 | -0.5% | [-1.1%, +0.3%] | -0.5%, +0.4%, -0.8% |
| node/postgres/window-64 | 524.45 | 518.19 | -1.2% | [-1.4%, -0.5%] | -1.1%, +0.6%, -1.2% |
| node/sqlite/read-1 | 116.28 | 115.74 | -0.5% | [-1.3%, -0.1%] | -0.8%, -0.5%, -0.1% |
| node/sqlite/read-50 | 164.66 | 165.50 | +0.5% | [-0.3%, +0.7%] | +0.2%, +0.5%, +0.2% |
| node/sqlite/read-1000 | 1124.49 | 1123.90 | -0.1% | [-0.4%, +0.3%] | +1.5%, -0.1%, -0.1% |
| node/sqlite/projection | 111.42 | 110.91 | -0.5% | [-1.0%, +0.0%] | -1.2%, +0.0%, -0.5% |
| node/sqlite/nested-4 | 143.41 | 141.87 | -1.1% | [-1.6%, +0.2%] | +0.8%, -1.1%, -1.5% |
| node/sqlite/nested-16 | 240.85 | 239.20 | -0.7% | [-1.3%, -0.6%] | +0.1%, -0.7%, -1.9% |
| node/sqlite/shared | 127.66 | 125.84 | -1.4% | [-2.6%, -1.0%] | -1.2%, -1.4%, -1.7% |
| node/sqlite/recursive | 481.14 | 479.48 | -0.3% | [-1.0%, -0.5%] | -1.3%, -0.3%, -0.8% |

Detected timing regressions in this matrix: ['node/postgres/read-1000'].
Runtimes with at least one detected timing gain: ['node', 'python'].
These exploratory intervals are not corrected for multiple comparisons; they do not establish production equivalence.
