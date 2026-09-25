# Contributing

## Setup

Install [`pre-commit`](https://pre-commit.com/), then install the hooks once with `make pre-commit-install`. It installs both the
`pre-commit` and the `commit-msg` hooks, so commit messages are checked when you commit, not when the release runs.

`make check` runs the full pre-commit suite on every file, and `make check-stage` runs it on the staged files only.

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
