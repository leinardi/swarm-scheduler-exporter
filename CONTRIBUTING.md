# Contributing

## Setup

Install [`pre-commit`](https://pre-commit.com/), then install the hooks once with `make pre-commit-install`. It installs both the
`pre-commit` and the `commit-msg` hooks, so commit messages are checked when you commit, not when the release runs.

`make check` runs the full pre-commit suite on every file, and `make check-stage` runs it on the staged files only.

## Integration tests

`make go-test-integration` builds the exporter and runs it against a throwaway Swarm: one manager and two workers, each a
privileged `docker:dind` container on your Docker daemon, joined over a dedicated bridge network. The tests live in
`test/integration` behind the `integration` build tag, the harness in `internal/testenv`. The same suite runs on every pull
request (`.github/workflows/integration.yaml`).

Prerequisites: a Docker daemon that allows privileged containers. The host may itself be a Swarm node: the test Swarm lives
entirely inside the DinD containers, and nothing on the host daemon is touched except the labelled containers and network the run
creates and removes. The nodes never pull from a registry: the workload image is pulled once on the host and loaded into each of
them.

```sh
make go-test-integration                            # the whole suite
make go-test-integration RUN='^TestNode_'           # only the tests matching a regexp
make go-test-integration UPDATE=1 RUN=TestSnapshot_ # rewrite the golden files under test/integration/testdata
make go-vet-integration                             # go vet, including the integration-tagged code
```

- `SSE_IT_KEEP_ON_FAILURE=true` keeps the cluster running when bring-up or a test fails, and prints the manager's address
  (`DOCKER_HOST=tcp://127.0.0.1:<port> docker node ls`) and the command that removes it.
- `SSE_IT_WORKERS` changes the number of workers, `SSE_IT_DIND_IMAGE` the `docker:dind` image, and `SSE_IT_SUITE_TIMEOUT`
  (default `12m`) the deadline after which every waiting test fails and teardown runs.

Every container and network a run creates carries the `swarm-scheduler-exporter.it.envid` label. At start-up the run logs a
cleanup command for its own resources (`docker ps -aq --filter label=swarm-scheduler-exporter.it.envid=<id> | xargs -r docker rm -fv; …`),
so a run killed half-way can still be cleaned up by hand, and each run removes environments older than an hour. `make
sweep-test-leaks` removes every labelled environment, whatever its age — do not run it while another run is in progress.

## Commit messages

All commits must follow [Conventional Commits 1.0.0](https://www.conventionalcommits.org/en/v1.0.0/) with a scope:
`<type>(<scope>)[!]: <description>`. The `conventional-pre-commit` hook enforces this on `commit-msg`, and the
`conventional-commits` CI job checks it again on every pull request. Release notes are generated from these messages.

Common types: `feat`, `fix`, `docs`, `test`, `refactor`, `perf`, `build`, `ci`, `chore`, `style`, `revert`. Use a lower-case,
imperative description:

```
fix(collector): drop stale task metrics when a service is removed
fix(deps): bump the Docker client to pick up an upstream fix
feat(metrics)!: rename the replica gauges
```

Mark breaking changes with `!` before the colon or a `BREAKING CHANGE: <description>` footer.

## Versioning

The release version is derived from these types: since the last release, any `feat` makes the next release a minor, any `fix` a
patch, and `!` or a `BREAKING CHANGE:` footer a major; `build`, `chore`, `ci`, `docs`, `refactor`, `test` and the rest bump nothing.
See [`docs/release.md`](docs/release.md).

Pull requests are merged with merge commits; squash and rebase merging are disabled. Every commit in a pull request therefore lands
on `master` as it is and counts toward the version, so each commit needs a correct type, not just the pull request as a whole. The
`conventional-commits` CI job checks every one of them. Enabling squash merging would make the pull-request title the commit
subject instead, and would need a CI check on pull-request titles first.
