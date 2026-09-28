#!/usr/bin/env bash
#
# MIT License
#
# Copyright (c) 2025 Roberto Leinardi
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

# Tests for binary-attestation-status.sh. Plain bash: a stub gh, first on PATH, answers every call
# from the STUB_MODES environment variable, a space-separated list of <sha256>=<mode>.
#
# Run: bash .github/scripts/binary-attestation-status.test.sh

set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
script="$here/binary-attestation-status.sh"

repo=leinardi/swarm-scheduler-exporter
signer_workflow=leinardi/swarm-scheduler-exporter/.github/workflows/release.yaml
source_ref=refs/heads/master
source_digest=0123456789abcdef0123456789abcdef01234567

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir "$work/bin"

cat >"$work/bin/gh" <<'STUB'
#!/usr/bin/env bash
# Stub gh: answers `gh api repos/<repo>/attestations/sha256:<digest>` and
# `gh attestation verify <file> ...` from STUB_MODES, and logs every call to STUB_LOG.
set -euo pipefail

echo "$*" >>"$STUB_LOG"

mode_for() {
	local entry
	for entry in $STUB_MODES; do
		if [ "${entry%%=*}" = "$1" ]; then
			echo "${entry#*=}"
			return
		fi
	done
	echo "stub gh: no mode for $1" >&2
	exit 99
}

case "$1 $2" in
"api repos/"*)
	mode=$(mode_for "${2##*sha256:}")
	case "$mode" in
	absent)
		printf '%s' '{"message":"Not Found","documentation_url":"https://docs.github.com/rest/repos/attestations#list-attestations","status":"404"}'
		echo 'gh: Not Found (HTTP 404)' >&2
		exit 1
		;;
	empty)
		echo '{"attestations":[]}'
		;;
	present | unverified)
		echo '{"attestations":[{"bundle":{},"repository_id":1}]}'
		;;
	http*)
		code=${mode#http}
		printf '{"message":"stub error","status":"%s"}' "$code"
		echo "gh: stub error (HTTP $code)" >&2
		exit 1
		;;
	network)
		echo 'Get "https://api.github.com/": dial tcp: lookup api.github.com: no such host' >&2
		exit 1
		;;
	malformed)
		echo '{"attestations": ['
		;;
	esac
	;;
"attestation verify")
	mode=$(mode_for "$(sha256sum "$3" | cut -d' ' -f1)")
	if [ "$mode" != present ]; then
		echo "stub gh: verification failed" >&2
		exit 1
	fi
	;;
*)
	echo "stub gh: unexpected call: $*" >&2
	exit 98
	;;
esac
STUB
chmod +x "$work/bin/gh"

export PATH="$work/bin:$PATH"
export STUB_LOG="$work/gh.log"
export STUB_MODES=""

failures=0

# binary <name> writes a file of its own content and prints its path.
binary() {
	printf 'binary %s\n' "$1" >"$work/$1"
	echo "$work/$1"
}

digest_of() {
	sha256sum "$1" | cut -d' ' -f1
}

# check <name> <want-exit> <want-stdout> <file>... runs the script on the files. want-exit is 0,
# 2 (usage) or "fail" (any other non-zero status, with an error naming the first file).
check() {
	local name=$1 want_exit=$2 want_stdout=$3
	shift 3

	local status=0 stdout
	: >"$STUB_LOG"
	stdout=$("$script" "$repo" "$signer_workflow" "$source_ref" "$source_digest" "$@" 2>"$work/stderr") || status=$?

	local ok=true
	case "$want_exit" in
	fail)
		if [ "$status" -eq 0 ] || [ "$status" -eq 2 ] || ! grep -qF "$1" "$work/stderr"; then
			ok=false
		fi
		;;
	*)
		if [ "$status" -ne "$want_exit" ]; then
			ok=false
		fi
		;;
	esac

	if [ "$stdout" != "$want_stdout" ]; then
		ok=false
	fi

	if [ "$ok" = true ]; then
		echo "ok   $name"
	else
		echo "FAIL $name: exit $status (want $want_exit), stdout [$stdout] (want [$want_stdout]), stderr [$(cat "$work/stderr")]"
		failures=$((failures + 1))
	fi
}

absent=$(binary absent)
empty=$(binary empty)
present=$(binary present)
unverified=$(binary unverified)
malformed=$(binary malformed)
network=$(binary network)

STUB_MODES="$(digest_of "$absent")=absent $(digest_of "$empty")=empty $(digest_of "$present")=present"
STUB_MODES+=" $(digest_of "$unverified")=unverified $(digest_of "$malformed")=malformed"
STUB_MODES+=" $(digest_of "$network")=network"

for code in 401 403 429 500; do
	file=$(binary "http$code")
	STUB_MODES+=" $(digest_of "$file")=http$code"
done

check "absent is printed" 0 "$absent" "$absent"
check "an empty attestations list is printed" 0 "$empty" "$empty"
check "present and verified is not printed" 0 "" "$present"

# The verification must pin the workflow, the branch it was given and the commit. The same
# script serves repositories whose default branch is master and repositories whose default
# branch is main, so both are checked.
for source_ref in refs/heads/master refs/heads/main; do
	: >"$STUB_LOG"
	check "present and verified on ${source_ref#refs/heads/} is not printed" 0 "" "$present"
	want_verify="attestation verify $present --repo $repo --signer-workflow $signer_workflow --source-ref $source_ref --source-digest $source_digest"
	if grep -qxF "$want_verify" "$STUB_LOG"; then
		echo "ok   present is verified against the signer workflow, ${source_ref#refs/heads/} and the commit"
	else
		echo "FAIL present is verified against the signer workflow, ${source_ref#refs/heads/} and the commit: calls [$(cat "$STUB_LOG")]"
		failures=$((failures + 1))
	fi
done
source_ref=refs/heads/master

check "present but not verified fails" fail "" "$unverified"

for code in 401 403 429 500; do
	check "HTTP $code fails" fail "" "$work/http$code"
done

check "a network error fails" fail "" "$network"
check "malformed JSON fails" fail "" "$malformed"
check "one absent and one present: only the absent one is printed" 0 "$absent" "$present" "$absent"
check "no file arguments is a usage error" 2 ""

if [ "$failures" -ne 0 ]; then
	echo "$failures case(s) failed" >&2
	exit 1
fi

echo "all cases passed"
