# Test fixtures (test-only data)

Everything under `configs/` is **fake test data**. No file here is a usable
WireGuard config: every key is `base64(SHA-256("wireguard fixture <label>"))`,
derived from a fixed label, so it is public and deterministic.

| Directory | Contract checked by `tests/prop_fixtures.rs` |
|---|---|
| `configs/valid/` | `validate_basic_wireguard_config` accepts the file, it runs no root hooks, and parse/render round-trips |
| `configs/hooks/` | Valid syntax, but `config_runs_scripts` is true (the helper refuses these) |
| `configs/invalid/` | `validate_basic_wireguard_config` rejects the file |

Do not edit the files by hand. Change `scripts/gen-fixtures.sh`, then run:

```sh
scripts/gen-fixtures.sh          # rewrite configs/
scripts/gen-fixtures.sh --check  # what CI runs: fail if configs/ differs
```

The repository `.gitignore` ignores `*.conf`; `tests/fixtures/.gitignore`
re-includes these scripted fixtures only. Never place a real config or key here.
The fuzz targets in `fuzz/` use `configs/` as their seed corpus.
