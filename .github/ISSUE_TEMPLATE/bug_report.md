---
name: Bug report
about: Report a bug or unexpected behavior
title: "[Bug]: "
labels: ["bug"]
assignees: []
---
## Step 1: Are you in the right place?

* [ ] I have checked that there are no duplicate active or recent issues (bugs, questions, or
  feature requests) describing this problem.
* [ ] I am using the latest release (the binary, or
  `ghcr.io/leinardi/swarm-scheduler-exporter:latest`), or have tested a build of `master`.
* [ ] This is not a security vulnerability (those go through the private process in SECURITY.md).

## Step 2: Describe your environment

<!-- The version is in the exporter's startup line (`swarm-scheduler-exporter starting
version=... commit=...`) and in the swarm_exporter_build_info metric; the binary has no
--version. -->

* swarm-scheduler-exporter version and commit (startup line or `swarm_exporter_build_info`): `?`
* How it runs (release binary, image tag and digest, own build) and where (Docker Swarm service,
  Docker Compose, other), and how it reaches the Docker API (socket bind mount, socket proxy,
  `DOCKER_HOST`): `?`
* Docker Engine version (`docker version`) and Swarm size (managers and workers): `?`
* Host operating system and architecture: `?`

## Step 3: Describe the problem

### Steps to reproduce

1. ---
2. ---
3. ---

### Observed results

<!-- What happened? This could be a description, error message, or log output. -->

*

### Expected results

<!-- What did you expect to happen? -->

*

### Relevant code, configuration, or data

<!-- Paste the smallest service definition or command line that reproduces the issue, and the
metric lines that look wrong. Never paste a secret, such as a TLS key or a registry token. -->

```yaml
# Example snippet (replace with your own)
```

### Error / log output

<!-- If you are getting an error, paste the output here. -->

```text
<paste logs or error messages here>
```

<!-- Adding screenshots, screen recordings, or additional context is always helpful. -->
