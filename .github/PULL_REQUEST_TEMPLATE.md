<!-- markdownlint-disable MD041 -->
<!--
Any HTML comment will be stripped when the markdown is rendered, so you don't need to delete them.
-->

## Summary

<!-- Short description of what this PR does. -->

## Pull request checklist

- [ ] I am targeting the `master` branch
- [ ] I have **rebased** this branch on top of the destination branch
- [ ] I have executed `make check` locally *before creating the commit* and it has run successfully
- [ ] Every commit message follows
  [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) with a scope
  (`type(scope): subject`)
- [ ] Each commit type matches whether the change ships: `fix` or `feat` for a change to the binary
  or the image that users should receive, a non-release type for everything else (`svu` derives
  the version from them; see `AGENTS.md`)
- [ ] I have performed a self-review of my own code
- [ ] There are no `WIP` commits in this PR

## Type of changes

<!-- Tick all that apply: -->

- [ ] 🐛 Bug fix
- [ ] ✨ New feature
- [ ] 🔧 Refactoring
- [ ] 📜 Docs
- [ ] 🧰 CI / tooling / infra
- [ ] Other (describe in Summary)
