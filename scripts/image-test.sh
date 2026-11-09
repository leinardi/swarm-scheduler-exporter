#!/usr/bin/env bash
#
# MIT License
#
# Copyright (c) 2026 Roberto Leinardi
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
#
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
#
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
# IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
# FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
# AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
# LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
# OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
# SOFTWARE.
#

# image-test.sh — tests a built swarm-scheduler-exporter image of one platform, in three tiers,
# each including the one before it:
#
#   smoke    the release's short test: the image is the platform's, its binary is byte for byte
#            the given release binary (sha256), and the container, started with no Docker socket,
#            logs the expected stamps in its startup line and serves them on /metrics as
#            swarm_exporter_build_info. The binary has no --version; without a Docker daemon its
#            workers retry while the HTTP server answers.
#   reduced  CI's arm64 test, under QEMU: smoke, plus the binary's ELF machine is the platform's
#            and the binary is static (no interpreter, no shared library), the binary's mode and
#            owner in the image are 0755 root:root, and the image config (the fixed non-root user,
#            entrypoint, exposed port, health check, labels) is what the Dockerfile writes.
#   full     CI's amd64 test: reduced, plus the container hardened as docker-compose.yaml runs it
#            and more (read-only root, no-new-privileges, every capability dropped) serves /metrics,
#            reports itself not ready on /healthz with no Docker daemon to reach (HTTP 503), and so
#            its own --healthcheck probe exits 1, stops cleanly on SIGTERM (exit code 0), and an
#            invalid flag value fails the start with a non-zero exit code.
#
# Usage:
#   scripts/image-test.sh --tier smoke|reduced|full --image IMAGE --platform os/arch \
#     --binary FILE [--expect-version "VERSION COMMIT DATE"]
#
# IMAGE must already be in the local Docker image store, for PLATFORM. Every container it starts
# is removed on exit. Needs docker, curl, sha256sum, tar and (reduced, full) readelf.

set -euo pipefail
# readelf and tar print in the C locale, the one the checks below read.
export LC_ALL=C

BINARY_PATH="usr/local/bin/swarm-scheduler-exporter"
PORT=8888

usage() {
	sed -n '/^# Usage:/,/^# IMAGE/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
	exit 2
}

die() {
	echo "image-test: FAIL: $*" >&2
	exit 1
}

pass() {
	echo "image-test: ok: $*"
}

tier="" image="" platform="" binary="" expect_version=""
while [[ $# -gt 0 ]]; do
	case "$1" in
	--tier) tier="${2:-}" ;;
	--image) image="${2:-}" ;;
	--platform) platform="${2:-}" ;;
	--binary) binary="${2:-}" ;;
	--expect-version) expect_version="${2:-}" ;;
	*) usage ;;
	esac
	shift 2 || usage
done
case "${tier}" in
smoke) level=1 ;;
reduced) level=2 ;;
full) level=3 ;;
*) usage ;;
esac
[[ -n ${image} && -n ${platform} && -n ${binary} ]] || usage
[[ -f ${binary} ]] || die "the release binary ${binary} does not exist"
want_version="" want_commit="" want_date=""
if [[ -n ${expect_version} ]]; then
	read -r want_version want_commit want_date <<<"${expect_version}"
	[[ -n ${want_date} ]] || die "--expect-version takes \"VERSION COMMIT DATE\", got '${expect_version}'"
fi

work="$(mktemp -d)"
containers=()
cleanup() {
	if [[ ${#containers[@]} -gt 0 ]]; then
		docker rm -f "${containers[@]}" >/dev/null 2>&1 || true
	fi
	rm -rf "${work}"
}
trap cleanup EXIT

# Sets NAME to a unique container name, which the exit trap removes. Not called through $( ): a
# subshell would add the name to its own copy of the list.
new_container() {
	NAME="sse-image-test-$1-$$-${RANDOM}"
	containers+=("${NAME}")
}

# --- The image is the platform's -------------------------------------------------------------

got_platform="$(docker image inspect -f '{{.Os}}/{{.Architecture}}' "${image}")"
[[ ${got_platform} == "${platform}" ]] || die "${image} is ${got_platform}, want ${platform}"
pass "image platform ${got_platform}"

# --- The binary in the image is the release binary --------------------------------------------

# The image's filesystem, exported from a container that never starts, so the binary's bytes,
# mode and owner are read without running anything.
new_container export
export_name="${NAME}"
docker create --platform "${platform}" --name "${export_name}" "${image}" >/dev/null
docker export -o "${work}/rootfs.tar" "${export_name}"
mkdir "${work}/rootfs"
tar -xf "${work}/rootfs.tar" -C "${work}/rootfs" "${BINARY_PATH}"
extracted="${work}/rootfs/${BINARY_PATH}"

want_sum="$(sha256sum "${binary}")"
want_sum="${want_sum%% *}"
got_sum="$(sha256sum "${extracted}")"
got_sum="${got_sum%% *}"
[[ ${got_sum} == "${want_sum}" ]] ||
	die "/${BINARY_PATH} in the image is sha256 ${got_sum}, the release binary ${binary} is ${want_sum}"
pass "/${BINARY_PATH} is the release binary (sha256 ${got_sum})"

# --- The exporter starts and reports its stamps -----------------------------------------------

# Starts the image as $1 with the metrics port published on a free loopback port, and any further
# docker run options; prints the host port. No Docker socket: the workers retry, the server serves.
start_exporter() {
	local name="$1"
	shift
	docker run -d --platform "${platform}" --name "${name}" -p "127.0.0.1::${PORT}" "$@" \
		"${image}" >/dev/null
	local mapping
	mapping="$(docker port "${name}" "${PORT}/tcp")"
	mapping="${mapping%%$'\n'*}"
	printf '%s' "${mapping##*:}"
}

# Waits up to 60 s (QEMU is slow) for the startup line of container $1 and prints it; dies when the
# container exits or never logs it.
await_startup_line() {
	local name="$1" logs line running
	for _ in $(seq 1 60); do
		logs="$(docker logs "${name}" 2>&1)"
		if line="$(grep -m1 -F 'swarm-scheduler-exporter starting' <<<"${logs}")"; then
			printf '%s' "${line}"
			return 0
		fi
		running="$(docker inspect -f '{{.State.Running}}' "${name}")"
		if [[ ${running} != true ]]; then
			printf '%s\n' "${logs}" >&2
			die "the container ${name} exited before it logged its startup line"
		fi
		sleep 1
	done
	docker logs "${name}" >&2 || true
	die "the container ${name} logged no startup line within 60 s"
}

# Waits up to 60 s for /metrics of container $1 on host port $2 to answer, and writes the body to
# $3; dies when the container exits or never answers.
await_metrics() {
	local name="$1" port="$2" out="$3" running
	for _ in $(seq 1 60); do
		if curl -fsS --max-time 5 -o "${out}" "http://127.0.0.1:${port}/metrics" 2>/dev/null; then
			return 0
		fi
		running="$(docker inspect -f '{{.State.Running}}' "${name}")"
		if [[ ${running} != true ]]; then
			docker logs "${name}" >&2 || true
			die "the container ${name} exited before /metrics answered"
		fi
		sleep 1
	done
	docker logs "${name}" >&2 || true
	die "/metrics of ${name} did not answer within 60 s"
}

hardening=()
if ((level >= 3)); then
	# docker-compose.yaml's hardening, plus every capability dropped: the exporter needs none.
	hardening=(--read-only --tmpfs /tmp --security-opt no-new-privileges:true --cap-drop ALL)
fi
new_container exporter
exporter="${NAME}"
port="$(start_exporter "${exporter}" "${hardening[@]}")"

line="$(await_startup_line "${exporter}")"
if [[ -n ${expect_version} ]]; then
	for pair in "version=${want_version}" "commit=${want_commit}" "date=${want_date}"; do
		key="${pair%%=*}" want="${pair#*=}"
		[[ ${line} == *"${key}=\"${want}\""* ]] ||
			die "the startup line does not report ${key} ${want}: ${line}"
	done
fi
pass "startup line: ${line}"

await_metrics "${exporter}" "${port}" "${work}/metrics"
build_info="$(grep -m1 '^swarm_exporter_build_info{' "${work}/metrics")" ||
	die "/metrics has no swarm_exporter_build_info sample"
if [[ -n ${expect_version} ]]; then
	for pair in "version=${want_version}" "commit=${want_commit}" "date=${want_date}"; do
		key="${pair%%=*}" want="${pair#*=}"
		[[ ${build_info} == *"${key}=\"${want}\""* ]] ||
			die "swarm_exporter_build_info does not carry ${key}=\"${want}\": ${build_info}"
	done
fi
pass "/metrics: ${build_info}"

if ((level >= 2)); then
	# --- The binary is the platform's, and static ------------------------------------------

	case "${platform}" in
	linux/amd64) want_machine="Advanced Micro Devices X86-64" ;;
	linux/arm64) want_machine="AArch64" ;;
	*) die "no ELF machine known for ${platform}" ;;
	esac
	header="$(readelf -h "${extracted}")"
	[[ ${header} == *"Machine:"*"${want_machine}"* ]] ||
		die "/${BINARY_PATH} is not a ${want_machine} binary"
	program_headers="$(readelf -lW "${extracted}")"
	[[ ${program_headers} != *INTERP* ]] || die "/${BINARY_PATH} has a program interpreter: it is not static"
	dynamic="$(readelf -dW "${extracted}")"
	[[ ${dynamic} != *"(NEEDED)"* ]] || die "/${BINARY_PATH} needs a shared library: it is not static"
	pass "/${BINARY_PATH} is a static ${want_machine} binary"

	# --- The binary's mode and owner ---------------------------------------------------------

	listing="$(tar --numeric-owner -tvf "${work}/rootfs.tar" "${BINARY_PATH}")"
	read -r mode owner _ <<<"${listing}"
	[[ ${mode} == -rwxr-xr-x && ${owner} == 0/0 ]] ||
		die "/${BINARY_PATH} is ${mode} ${owner}, want -rwxr-xr-x 0/0"
	pass "/${BINARY_PATH} is ${mode} ${owner}"

	# --- The image config ----------------------------------------------------------------------

	user="$(docker image inspect -f '{{.Config.User}}' "${image}")"
	[[ ${user} == 65532:65532 ]] || die "the image runs as '${user:-root}', want 65532:65532"
	entrypoint="$(docker image inspect -f '{{json .Config.Entrypoint}}' "${image}")"
	[[ ${entrypoint} == '["/usr/local/bin/swarm-scheduler-exporter"]' ]] ||
		die "the entrypoint is ${entrypoint}, want [\"/usr/local/bin/swarm-scheduler-exporter\"]"
	ports="$(docker image inspect -f '{{json .Config.ExposedPorts}}' "${image}")"
	[[ ${ports} == "{\"${PORT}/tcp\":{}}" ]] || die "the exposed ports are ${ports}, want ${PORT}/tcp only"
	healthcheck="$(docker image inspect -f '{{json .Config.Healthcheck.Test}}' "${image}")"
	[[ ${healthcheck} == '["CMD","/usr/local/bin/swarm-scheduler-exporter","--healthcheck"]' ]] ||
		die "the health check is ${healthcheck}, want the binary's own --healthcheck"
	if [[ -n ${expect_version} ]]; then
		for pair in "version=${want_version}" "revision=${want_commit}" "created=${want_date}"; do
			key="${pair%%=*}" want="${pair#*=}"
			got="$(docker image inspect -f "{{ index .Config.Labels \"org.opencontainers.image.${key}\" }}" "${image}")"
			[[ ${got} == "${want}" ]] || die "the label org.opencontainers.image.${key} is '${got}', want '${want}'"
		done
	fi
	pass "image config: user ${user}, entrypoint ${entrypoint}, ports ${ports}, health check, labels"
fi

if ((level >= 3)); then
	# --- Not ready without a Docker daemon -------------------------------------------------------

	status="$(curl -sS --max-time 5 -o /dev/null -w '%{http_code}' "http://127.0.0.1:${port}/healthz")"
	[[ ${status} == 503 ]] || die "/healthz with no Docker daemon answered HTTP ${status}, want 503"
	pass "/healthz with no Docker daemon: HTTP 503"

	# The image has no shell: the probe is the binary itself, exec'd in the running container.
	probe_code=0
	docker exec "${exporter}" /usr/local/bin/swarm-scheduler-exporter --healthcheck >/dev/null 2>&1 ||
		probe_code=$?
	[[ ${probe_code} == 1 ]] || die "--healthcheck of an unready exporter exited ${probe_code}, want 1"
	pass "--healthcheck of an unready exporter: exit code 1"

	# --- A stop is clean -------------------------------------------------------------------------

	docker stop -t 10 "${exporter}" >/dev/null
	exit_code="$(docker inspect -f '{{.State.ExitCode}}' "${exporter}")"
	[[ ${exit_code} == 0 ]] || die "docker stop: exit code ${exit_code}, want 0"
	pass "docker stop: exit code 0"

	# --- An invalid flag fails the start -------------------------------------------------------

	new_container invalid
	invalid="${NAME}"
	docker run --platform "${platform}" --name "${invalid}" "${image}" --poll-delay 0s >/dev/null 2>&1 || true
	exit_code="$(docker inspect -f '{{.State.ExitCode}}' "${invalid}")"
	[[ ${exit_code} != 0 ]] || die "an invalid --poll-delay exited 0"
	pass "an invalid --poll-delay fails the start (exit code ${exit_code})"
fi

echo "image-test: ${tier}: every check passed for ${image} (${platform})"
