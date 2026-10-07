| case | before µs | after µs | change | 95% | per-process |
| node/postgres/definition | 240.39 | 251.67 | +4.7% | [+11.2, +41.7] | +22.9, +4.7, +31.4 |
| node/postgres/plan-native | 2.20 | 2.29 | +3.9% | [-2.5, +12.7] | +3.9, -33.0, +11.2 |
| node/postgres/construct+sql | 12.36 | 14.23 | +15.1% | [-15.8, +13.3] | -4.2, +15.1, +2.2 |
| node/sqlite/definition | 190.23 | 264.36 | +39.0% | [+13.2, +39.0] | +32.6, +42.0, +21.6 |
| node/sqlite/plan-native | 2.58 | 2.98 | +15.4% | [+13.1, +16.7] | -13.0, +15.9, +15.4 |
| node/sqlite/construct+sql | 7.12 | 12.92 | +81.4% | [-3.0, +55.9] | -18.9, +102.0, +15.6 |
| python/postgres/definition | 303.57 | 331.21 | +9.1% | [+12.8, +33.8] | +3.5, +32.1, +22.7 |
| python/postgres/plan-native | 1.10 | 1.22 | +10.6% | [+9.0, +16.5] | +4.3, +14.3, +13.9 |
| python/postgres/construct+sql | 12.40 | 13.24 | +6.8% | [-13.8, +10.9] | -1.7, +14.1, -7.2 |
| python/sqlite/definition | 330.38 | 370.89 | +12.3% | [-0.6, +31.2] | +22.2, +12.3, +10.3 |
| python/sqlite/plan-native | 1.10 | 1.27 | +14.9% | [+8.3, +18.8] | +9.6, +112.2, +15.9 |
| python/sqlite/construct+sql | 9.64 | 11.01 | +14.2% | [-5.3, +13.5] | +6.9, +14.4, -6.6 |
