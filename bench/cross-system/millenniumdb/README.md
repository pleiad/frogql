# MillenniumDB — cross-system bench adapter

[MillenniumDB](https://github.com/MillenniumDB/MillenniumDB) (IMFD) is a
C++ graph database with an early implementation of GQL. This adapter plugs
it into the cross-system bench next to `kuzu/`, `grafeo/` and the rest.

- **Version measured:** a source build of `dev`. The runner was validated
  against commit `d67f3e7` (2026-11-11). `run_info.txt` records the binary
  and its commit for every run.
- **Backend label:** `millenniumdb-gql`.
- **Interface measured:** the HTTP endpoint (`POST /`, TSV result) of a
  `mdb server` the runner starts and stops itself. Not the Python driver:
  it crashes the server on the second query of a session. See
  `DIVERGENCES.md` §3.
- **ICs:** IC2, IC8, IC11, IC12, IC13. Why the other eight do not run is in
  `DIVERGENCES.md`, with a query reproducing each engine bug.

## Build MillenniumDB (once per machine)

There is no wheel. Build the server from source; MillenniumDB's own script
installs the system packages (it calls `sudo apt-get`) and builds into
`build/Release/`:

```bash
git clone https://github.com/MillenniumDB/MillenniumDB
cd MillenniumDB
./scripts/setup_millenniumdb.sh
export MDB_BIN=$PWD/build/Release/bin/mdb
```

Without `sudo`, install `g++ cmake libssl-dev libncurses-dev libicu-dev`
some other way and pass `--skip-deps`. The script also downloads the Boost
1.82 headers into `third_party/`.

The runner finds the binary through `$MDB_BIN`, then `mdb` on `PATH`, then
a checkout next to this repository (`../MillenniumDB/build/Release/bin/mdb`
or `../MillenniumDB/build/bin/mdb`).

## Files

| File | Role |
|---|---|
| `setup.py` | Converts the LDBC SF0.1 CSVs into MillenniumDB's GQL import format (`ldbc-sf01.gql`), then runs `mdb import`. Mirrors froGQL's LDBC loader rule for rule. IC-agnostic. |
| `run.py` | Starts `mdb server`, runs one IC for every parameter row over HTTP, emits the per-iteration CSV and the row-hash oracle, stops the server. Standard library only. |
| `mdb_common.py` | Paths and the `mdb` binary lookup shared by both. |
| `ic<n>.gql` | Per-IC translations of `bench/ldbc-queries/ic<n>.toml`. |
| `blocked/ic<n>.gql` | Complete translations MillenniumDB cannot answer correctly yet; each says why. |
| `DIVERGENCES.md` | Every dialect change, the engine bugs found, and what each IC checked out as. |

## Setup cost

`setup.py` writes the 130 MB dump in ~8 s; `mdb import` is the long part.
On an Apple M3 it took about 11 minutes of CPU, most of it writing the
B+tree indexes. The database is ~96 MB.

## Server and memory

`run.py` launches `mdb server <db> --port <free port> --browser false
--timeout 3600` as its own child, so `_lib/memrun.py` (which samples the
runner's process group) counts the server's RSS and kills it with the
runner. Each IC starts a fresh server outside the timed loop; the warmup
iterations absorb the cold page cache, as for every other system.

The server's own `--timeout` is not honoured for every query (§3, item 9
of `DIVERGENCES.md`), so keep the orchestrator's `--timeout-s` on.
