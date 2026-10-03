# Run from the repository root after building both release bindings.
def check [] {
  if $env.LAST_EXIT_CODE != 0 { error make {msg: 'Benchmark command failed'} }
}

def main [
  --url: string = 'postgres://postgres:postgres@127.0.0.1:55439/orm_test'
  --out: string = 'bench/shared-behavior/results'
] {
  $env.ORM_TEST_DATABASE_URL = $url
  mkdir $out
  for run in 1..3 {
    if ($run mod 2) == 1 {
      ^.venv/bin/python bench/shared-behavior/locks.py --out $'($out)/python-($run).json'
      check
      ^node bench/shared-behavior/locks.mjs --out $'($out)/node-($run).json'
      check
    } else {
      ^node bench/shared-behavior/locks.mjs --out $'($out)/node-($run).json'
      check
      ^.venv/bin/python bench/shared-behavior/locks.py --out $'($out)/python-($run).json'
      check
    }
    let cases = 'concurrent transactions,transaction+read+write,transaction+short'
    ^.venv/bin/python bench/shared-behavior/locks.py --connections 4 --only $cases --out $'($out)/python-pool4-($run).json'
    check
    ^node bench/shared-behavior/locks.mjs --connections 4 --only $cases --out $'($out)/node-pool4-($run).json'
    check
  }
  ^.venv/bin/python bench/shared-behavior/compat.py
  check
  ^.venv/bin/python bench/shared-behavior/report.py --directory $out --out $'($out)/summary.md' --check
  check
}
