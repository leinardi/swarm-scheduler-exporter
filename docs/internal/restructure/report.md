# Documentation restructure report

Stage 1 of the documentation rework: `docs/` becomes a published set plus an unpublished
`docs/internal/`. The plan is [`plan.toml`](plan.toml). Status: **plan approved by the owner and
applied; all 21 mappings reviewed by the owner; stage 1 awaits sign-off**.

## Baselines

- Base commit: `348137da15b136b678a0127e661b6c97004fbb88`.
- `blocks.json`: the content baseline of every Markdown file, taken on a clean tree.
- `urls.json`: source-derived, because the repository had no site. URL preservation can therefore
  only be **partially verified**: GitHub paths and heading anchors are covered, built routes are
  not.

## Classification

| File | Class | Lifecycle | Page type | Cited by | Outcome |
| --- | --- | --- | --- | --- | --- |
| `docs/metrics.md` | published | living | reference | README | stays |
| `docs/configuration.md` | published | living | reference | README, `metrics.md` | stays |
| `docs/deployment.md` | published | living | how-to | integration tests | stays, frozen path |
| `docs/operations.md` | published | living | explanation | Dockerfile (`Health`) | stays, frozen heading |
| `docs/alerts.md` | published | living | how-to | README | stays |
| `docs/migration.md` | published | living | how-to | `src/cli.rs`, `src/duration.rs` | stays, Q numbers frozen |
| `docs/release.md` | internal | living | runbook | workflows, Dockerfile, meta-test, agent files | moves |
| `docs/release.md`, Go 1.x notes | internal | record | history | none | splits to `history/` |

## New layout

```text
docs/
  index.md               landing page (stage 2)
  getting-started.md     new, from the README quick start (G8)
  deployment.md
  configuration.md
  metrics.md
  alerts.md
  operations.md
  migration.md
  verifying-releases.md  new, from the runbook's verification commands (G8)
  assets/                brand icon, favicons, social preview (stage 2)
  internal/
    README.md            what lives here and why
    release.md           the maintainer runbook
    history/go-releases.md
    restructure/         this record
```

The runbook keeps its verification commands; `verifying-releases.md` is the user-facing copy.

## Owner decisions

- **d1 (G3, G10):** move the runbook and update every citation, workflow messages included.
- **d2:** the docs state the current event-stream behaviour. The regression below was reported to
  rust-common privately and fixed upstream before this stage merged. This work changes no code.
- **d3:** contradicted claims are corrected in this stage, each rewording in `mappings.toml`.
  Changes to the example alert rules are shown separately.
- **d4 (G2, G8):** add Getting started, Verifying releases and the landing page.

## Code regression found, since fixed

`docs/operations.md` promised a 1 MiB limit per Docker event record. Commit `2379192` added that
limit (`MAX_EVENT_BYTES`) and commit `2d9e2df` removed it when the exporter adopted rust-common's
Docker client, so no tagged release enforced it. rust-common `v3.0.0` bounds the record again, and
`792b3e6` (#132) moved the pin, set the limit to 1 MiB (`EVENT_RECORD` in `src/docker/mod.rs`) and
rewrote the paragraph in `docs/operations.md`. This stage was rebased onto that change and keeps
its text, which was not audited again here.

## Claims the code contradicts

Every row was confirmed by opening both sides. Rows marked "Audit" were confirmed when their
correction was written. Row 9 was narrowed then: a failed test does print the manager's address;
the `docker node ls` command is never printed, and a bring-up failure prints no address. All rows
are corrected. Row 18 was approved by the owner (gate G10) and now names
`docker_client::test_util::FakeEngine`, the fake `tests/engine_wire.rs` uses.

| # | Claim | Code | Status |
| --- | --- | --- | --- |
| 1 | `operations.md:46`: 1 MiB event record limit | no limit then; fixed in #132 | Confirmed |
| 2 | `SECURITY.md:43`: every image pinned to a digest | `Dockerfile:52,148` pin `dhi.io` by tag | Confirmed |
| 3 | `alerts.md:48,65`: excludes `dead` with exit code 0 | only `exited` is inspected, `containers.rs:267` | Confirmed |
| 4 | `metrics.md:214`: `exit_code` only on `state="exited"` | set on all ten series, `containers.rs:363` | Confirmed |
| 5 | `migration.md:7`: inventory is the 1.3.2 exposition | `swarm_node_*`, `restart_policy` came later, `67f81a8` | Confirmed |
| 6 | `migration.md:41`: dual-stack on `:port` only | any wildcard host binds `[::]`, `server.rs:159` | Confirmed |
| 7 | `migration.md:27,89`: durations print `1m 30s` | log fields use `Duration` Debug, `run.rs:246` | Audit |
| 8 | `README.md:110`: must run on a manager | in the proxy stack only the proxy is constrained | Confirmed |
| 9 | `CONTRIBUTING.md:85`: failure prints the manager address | prints host and cleanup command | Audit |
| 10 | `SECURITY.md:5`: image tags are immutable | `:latest` and `:<major>` move, `release.yaml:1068` | Audit |
| 11 | `CONTRIBUTING.md:32`: `make verify` list | omits `rust-doc`, `.mk/rust.mk:150` | Audit |
| 12 | `metrics.md:206`: `service` from Compose labels | falls back to the Swarm name, `containers.rs:391` | Audit |
| 13 | `metrics.md:211`: `display_name` sources | omits project and container fallback | Audit |
| 14 | `configuration.md:53`: TLS verify "set to `1`" | any non-empty value enables it, `0` included | Audit |
| 15 | `migration.md:62,77`: startup failure on Q5 | applies to `tcp://` hosts only | Audit |
| 16 | `configuration.md:45`: `500ms` as a valid delay | parses, then exits 1 (below the minimum) | Audit |
| 17 | `CONTRIBUTING.md:82`: sample shows 22 tests | `TESTS` has 21 entries | Audit |
| 18 | `AGENTS.md:93`: `tests/support/fake_engine.rs` | no such file; the fake is rust-common's | Confirmed |

Example alert problems, fixed with the owner's approval:

- `SwarmServiceRollbackOngoing` included `rollback_completed`, which persists until the next
  update, so the alert kept firing after the rollback had ended. It now matches `rollback_started`.
- `ContainerUnhealthy` excluded `dead` containers with exit code 0, which cannot exist because only
  `exited` containers are inspected. It now excludes `exited` with code 0.
- The static `service:` rule labels overwrote the metric's `service` label. They are now
  `component:`, which changes the label any Alertmanager route copied from the examples matches.

## Not verifiable from the code

- The README's resource figures (binary and image sizes, RSS, 964 series): measurements.
- The proxy image's defaults and HAProxy's idle timeout (about 10 minutes, 660s observed).
- "Up to 24h" for a down node's task state: supported only by a code comment.
- Runtime behaviour was not exercised: nothing was built or run for this audit.
- `docs/release.md` was not audited line by line; it moves unchanged.

## Drift coverage

`docstool drift --scope repo --paths --leads` (toolkit 0.2.0) exits 3 because its make and env
coverage is partial. The gaps were checked by hand:

- **Make:** `.mk/docker.mk` and `.mk/rust.mk` define targets dynamically. All 25 targets the
  current docs name exist in `make -p`. The four other matches are prose ("make the API").
- **Env:** the two reads the tool cannot resolve are `src/main.rs:52`, which passes the process
  environment to `settings_from_env`, and `testenv/src/dind.rs:142`, which reads
  `SSE_IT_STALE_AFTER` (documented in `CONTRIBUTING.md`).
- **`DOCKER_TLS_VERIFY`:** read at `src/docker/mod.rs:151` and by rust-common's
  `docker-client/src/config.rs:169,176` (`v3.0.0`). The docs are correct.
- **`DOCKER_CERT_PATH`:** read at `src/docker/mod.rs:150` and `config.rs:182`. The docs were correct
  but incomplete; see decision D5.
- **`DHI_USERNAME`:** read by nothing, and no such repository or `release` environment variable
  exists. The workflows and Dependabot use the literal user name `leinardi`, so the runbook's "no
  longer read" is correct. No action.

### Decision D5

The owner approved completing the Docker TLS reference in `docs/configuration.md`, which omitted
four behaviours of `Config::from_env`:

- without `DOCKER_CERT_PATH`, the certificates are read from `$HOME/.docker`;
- with TLS on and neither `DOCKER_CERT_PATH` nor `HOME` set, startup fails;
- an `https://` host always uses TLS, without `DOCKER_TLS_VERIFY`;
- a non-empty `DOCKER_TLS` is refused at startup (`src/docker/tests.rs:112`), and no doc mentions
  it.

All four are now documented. Whether `DOCKER_TLS` also needs a `migration.md` entry depends on what
Go 1.x did with it, which was not checked.

## Checker leads dismissed

- `DOCKER_API_VERSION`: read at `src/docker/mod.rs:156` and by rust-common's
  `docker-client/src/config.rs`.
- `SSE_IT_KEEP_ON_FAILURE`: read by `testenv/src/cluster.rs`.
- `RUST_RUN_BIN`, `RUST_STAMP_PREFIX`: Makefile knobs read by `.mk/rust.mk`.
- `reconciler/recover.rs`: a path relative to `src/collector/`.
- `make go-vet`, `actions/setup-go`: in the historical Go 1.x notes.
