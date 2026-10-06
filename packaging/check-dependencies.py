"""Assert isolated binding graphs exclude unused drivers, TLS and tooling."""
import json
import subprocess

for binding in ("orm-python", "orm-node"):
    for profile in ("postgres", "sqlite", "combined", "tooling"):
        feature = f"profile-{profile}"
        tree = subprocess.check_output([
            "cargo", "tree", "-p", binding, "--no-default-features", "--features", feature,
            "--prefix", "none", "--format", "{p}", "--edges", "normal",
        ], text=True)
        packages = {line.split()[0] for line in tree.splitlines()}
        excluded = set()
        if profile == "postgres":
            excluded |= {"rusqlite", "libsqlite3-sys"}
        if profile == "sqlite":
            excluded |= {"tokio-postgres", "deadpool-postgres", "tokio-postgres-rustls", "rustls", "ring", "webpki-roots"}
        if profile != "tooling":
            excluded.add("orm-cli")
        assert not (packages & excluded), (binding, profile, packages & excluded)
        features = subprocess.check_output([
            "cargo", "tree", "-p", binding, "--no-default-features", "--features", feature,
            "-e", "features", "--prefix", "none",
        ], text=True)
        if profile == "sqlite":
            assert 'sea-query feature "backend-postgres"' not in features
        if profile == "postgres":
            assert 'sea-query feature "backend-sqlite"' not in features
        assert 'sea-query feature "backend-mysql"' not in features
        print(json.dumps({"binding": binding, "profile": profile, "packages": sorted(packages)}))
