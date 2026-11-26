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

ifndef MK_COMMON_RUST_INCLUDED
MK_COMMON_RUST_INCLUDED := 1

# A Cargo project or workspace. Every command runs workspace-wide (one package is a workspace of
# one), so a lint set declared once in [workspace.lints] reaches every member, and --locked
# everywhere: Cargo.lock is the pin, and a command that rewrote it would build a dependency graph
# nobody reviewed.

CARGO    ?= cargo
BIN_NAME ?= app
DIST_DIR ?= dist

# Version metadata (generic, derived from git when available), as go.mk has it.
GIT_COMMIT  ?= $(shell git rev-parse --short=12 HEAD 2>/dev/null || echo "none")
GIT_VERSION ?= $(shell git describe --tags --dirty --always 2>/dev/null || echo "v0.0.0")
BUILD_DATE  ?= $(shell date -u +"%Y-%m-%dT%H:%M:%SZ")

# HEAD's commit date in UTC, the date the release workflows stamp; empty outside git. BUILD_DATE
# stays the clock's by default; a consumer that wants a stamp that only moves with a commit, so a
# repeated build is a cache hit, sets before the include:
#   BUILD_DATE ?= $(or $(GIT_COMMIT_DATE),$(shell date -u +"%Y-%m-%dT%H:%M:%SZ"))
GIT_COMMIT_DATE ?= $(shell TZ=UTC git log -1 --format=%cd --date=format-local:%Y-%m-%dT%H:%M:%SZ 2>/dev/null)

# RUST_STAMP_PREFIX: the binary reads its version at compile time from <prefix>_VERSION,
# <prefix>_COMMIT and <prefix>_DATE (option_env! or env!), e.g. RUST_STAMP_PREFIX := MYAPP.
# Empty: no stamps. Only the builds (rust-build through RUST_BUILD_ENV) and rust-run stamp, so the
# tests see the binary's defaults.
# RUST_VERSION_ENV is the assignment list, for a consumer's own recipes.
RUST_STAMP_PREFIX ?=
RUST_VERSION_ENV = $(if $(strip $(RUST_STAMP_PREFIX)), \
  $(RUST_STAMP_PREFIX)_VERSION="$(GIT_VERSION)" \
  $(RUST_STAMP_PREFIX)_COMMIT="$(GIT_COMMIT)" \
  $(RUST_STAMP_PREFIX)_DATE="$(BUILD_DATE)")

# rust-build's stamps, RUST_VERSION_ENV by default. A crate that reads the stamps is a unit that
# rust-build and rust-test share, so a stamped rust-build and the unstamped tests recompile it
# after each other; `RUST_BUILD_ENV :=` builds it unstamped, as the tests do, and leaves the stamps
# to rust-dist and rust-run.
RUST_BUILD_ENV ?= $(RUST_VERSION_ENV)

# The features rust-build, rust-clippy and rust-doc compile; empty for the default features. The
# tests take the default features, so a feature whose tests need more than cargo (Docker, a
# device) stays out of rust-test while its code is still built and linted.
RUST_FEATURES ?= --all-features

# cargo-deny's scope; --workspace checks every member's graph, not only the root package's.
RUST_DENY_ARGS ?= --workspace

# The shipped graphs rust-deny-shipped checks the bans of, each on its own: a space-separated list
# of <manifest>:<config> pairs, a crate's Cargo.toml and the cargo-deny config its shipped graph is
# held to (crates/agent/Cargo.toml:deny-shipped-agent.toml). Neither may hold a space or a colon.
# Empty by default; rust-deny-shipped refuses an empty list rather than pass on nothing.
RUST_DENY_SHIPPED ?=

# rust-run's binary, for a package with more than one; empty lets cargo pick the only one.
RUST_RUN_BIN ?=

# Where cargo writes: its effective target directory (the binaries) and build directory (a build
# script's OUT_DIR, which is the target directory unless build.build-dir moves it), as cargo
# metadata reports them, so CARGO_TARGET_DIR, CARGO_BUILD_TARGET_DIR, build.target-dir and
# build.build-dir in a .cargo/config.toml all count. Those variables and CARGO_HOME (its
# config.toml counts too) are passed to cargo by name: GNU make before 4.4 runs $(shell) without
# the command line's and the exported ones, which the build's recipe has. Recursive and read once
# per make run, so cargo is asked only by a recipe that copies. The keys are matched where cargo
# prints them, before version and workspace_root, so a [workspace.metadata] or
# [package.metadata] key of the same name is not read. A path holding a space, a backslash or a
# double quote (escaped in the JSON) is not read: set RUST_TARGET_DIR (and, for rust-release.mk,
# RUST_RELEASE_TARGET_DIR) then.
_RUST_CARGO_ENV = $(foreach var,CARGO_HOME CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR CARGO_BUILD_BUILD_DIR,$(if $($(var)),$(var)="$($(var))"))
_RUST_METADATA = $(shell cd "$(or $(REPO_ROOT),.)" && $(_RUST_CARGO_ENV) $(CARGO) metadata --format-version 1 --no-deps 2>/dev/null | sed -n -e 's/.*"target_directory":"\([^"\\ ]*\)","build_directory":"\([^"\\ ]*\)","version":1,"workspace_root":".*/\1 \2/p' -e t -e 's/.*"target_directory":"\([^"\\ ]*\)","version":1,"workspace_root":".*/\1/p')
_rust_metadata = $(or $(_RUST_DIRS),$(eval _RUST_DIRS := $$(_RUST_METADATA))$(_RUST_DIRS))

# Where rust-dist finds the binary: cargo's target directory by default; a consumer may set it.
RUST_TARGET_DIR ?= $(or $(word 1,$(_rust_metadata)),$(error cannot read cargo's target directory (run cargo metadata to see why); set RUST_TARGET_DIR))

# The Cargo profile rust-dist builds: release by default; another one (say a local profile that
# inherits release with thin or no LTO, for a quicker edit-run loop) with --profile, from its own
# directory under RUST_TARGET_DIR (debug for the dev profile).
RUST_DIST_PROFILE ?= release
RUST_DIST_PROFILE_ARG = $(if $(filter release,$(RUST_DIST_PROFILE)),--release,--profile "$(RUST_DIST_PROFILE)")
RUST_DIST_PROFILE_DIR = $(if $(filter dev,$(RUST_DIST_PROFILE)),debug,$(RUST_DIST_PROFILE))

# rust-clean-stale's age limit in days: cargo-sweep removes the build artifacts not used for longer.
RUST_SWEEP_DAYS ?= 7

.PHONY: rust-build
rust-build: ## Build the Rust workspace, every target, with the version stamps
	$(RUST_BUILD_ENV) $(CARGO) build --workspace --all-targets $(RUST_FEATURES) --locked

.PHONY: rust-dist
rust-dist: ## Build the binary BIN_NAME in RUST_DIST_PROFILE (release), with the version stamps, into DIST_DIR
	$(RUST_VERSION_ENV) $(CARGO) build $(RUST_DIST_PROFILE_ARG) --locked --bin "$(BIN_NAME)"
	@mkdir -p "$(DIST_DIR)"
	cp "$(RUST_TARGET_DIR)/$(RUST_DIST_PROFILE_DIR)/$(BIN_NAME)" "$(DIST_DIR)/$(BIN_NAME)"

.PHONY: rust-run
rust-run: ## Run the binary with the version stamps (ARGS="...")
	$(RUST_VERSION_ENV) $(CARGO) run --locked $(if $(strip $(RUST_RUN_BIN)),--bin "$(RUST_RUN_BIN)") -- $(ARGS)

.PHONY: rust-test
rust-test: ## Run the Rust tests
	$(CARGO) test --workspace --locked

.PHONY: rust-clippy
rust-clippy: ## Lint the Rust workspace with clippy, warnings as errors
	$(CARGO) clippy --workspace --all-targets $(RUST_FEATURES) --locked -- -D warnings

.PHONY: rust-fmt
rust-fmt: ## Format the Rust code
	$(CARGO) fmt --all

.PHONY: rust-fmt-check
rust-fmt-check: ## Fail if the Rust code needs formatting
	$(CARGO) fmt --all --check

.PHONY: rust-doc
rust-doc: ## Check the Rust documentation, warnings as errors
	RUSTDOCFLAGS="$(RUSTDOCFLAGS) -D warnings" $(CARGO) doc --workspace $(RUST_FEATURES) --no-deps --locked

.PHONY: rust-deny
rust-deny: ## Check the Rust dependencies with cargo-deny (advisories, licences, bans, sources; network)
	$(CARGO) deny $(RUST_DENY_ARGS) check

# One rust-deny-shipped recipe line per pair, so make stops at the first that fails.
define _RUST_DENY_SHIPPED_LINE
	$(CARGO) deny --manifest-path "$(word 1,$(subst :, ,$(1)))" --config "$(word 2,$(subst :, ,$(1)))" check bans

endef

# The pairs that are not exactly <manifest>:<config>.
_rust_deny_shipped_bad = $(foreach p,$(RUST_DENY_SHIPPED),$(if $(filter 2,$(words $(subst :, ,$(p)))),,$(p)))

.PHONY: rust-deny-shipped
rust-deny-shipped: ## Check each shipped graph's bans with cargo-deny (RUST_DENY_SHIPPED)
	@test -n "$(strip $(RUST_DENY_SHIPPED))" || { echo "RUST_DENY_SHIPPED is empty; list the <manifest>:<config> pairs whose bans to check" >&2; exit 1; }
	@test -z "$(strip $(_rust_deny_shipped_bad))" || { echo "RUST_DENY_SHIPPED: not <manifest>:<config>: $(strip $(_rust_deny_shipped_bad))" >&2; exit 1; }
	$(foreach pair,$(RUST_DENY_SHIPPED),$(call _RUST_DENY_SHIPPED_LINE,$(pair)))

.PHONY: rust-clean-stale
rust-clean-stale: ## Remove build artifacts unused for RUST_SWEEP_DAYS (7) days or from removed toolchains (cargo-sweep)
	@command -v cargo-sweep >/dev/null 2>&1 || { \
	  echo "rust-clean-stale needs cargo-sweep: cargo install cargo-sweep --locked" >&2; exit 1; }
	$(CARGO) sweep --time "$(RUST_SWEEP_DAYS)"
	$(CARGO) sweep --installed

# rust-pin-bump: moves every tracked Cargo.toml pin on RUST_PIN_URL (a one-line inline table with
# git, tag and version) to TAG=vX.Y.Z, and the one line marked RUST_PIN_DOC_MARKER in each
# RUST_PIN_DOCS file (the current pin; historical mentions stay), then re-resolves Cargo.lock with
# plain cargo metadata (never cargo update) and checks the result: only the pin lines changed, the
# lock holds one revision of the URL, at TAG, and every package added or removed and every edge
# that moved lies in the rust-common closure, old or new, in cargo metadata --all-features and in
# Cargo.lock's full package list alike (a workspace member's own edge to a re-pinned package
# excepted). Any refusal or failure restores every file it touched; on success it reports what
# moved, a split between a consumer's dependency and rust-common's new one included (reported, not
# failed: a pin-only step need not build). It refuses a malformed or unchanged TAG and uncommitted
# changes to the manifests, the lock or the docs. It runs a Python 3 script (standard library
# only) through RUST_PIN_PYTHON.
RUST_PIN_URL ?=
RUST_PIN_DOCS ?=
RUST_PIN_DOC_MARKER ?= rust-pin-current
RUST_PIN_PYTHON ?= python3

# The script, one tab before each line, which _RUST_PIN_BUMP_RUN dedents and runs. checkmake
# still reads a line ending in a colon as a rule, so no two such lines may be the same (one try:).
_RUST_PIN_BUMP_RUN = import os, textwrap; exec(compile(textwrap.dedent(os.environ['_RUST_PIN_BUMP_SCRIPT']), 'rust-pin-bump', 'exec'))

define _RUST_PIN_BUMP_PY
	# rust-pin-bump: move every pin on RUST_PIN_URL to one tag, re-resolve, check that only the
	# rust-common closure moved, and restore every file on any failure. Standard library only.
	import argparse
	import json
	import os
	import re
	import shutil
	import signal
	import subprocess
	import sys
	import tempfile

	PROG = "rust-pin-bump"


	class Refusal(Exception):
	    pass


	def say(text):
	    print(PROG + ": " + text, file=sys.stderr)


	def run(args, capture=True):
	    result = subprocess.run(args, stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
	                            stderr=subprocess.PIPE, text=True)
	    if result.returncode != 0:
	        raise Refusal(" ".join(args) + " failed (" + str(result.returncode) + "): "
	                      + result.stderr.strip())
	    return result.stdout


	def normalize(url):
	    url = url.rstrip("/")
	    return url[:-4] if url.endswith(".git") else url


	def from_url(source, url):
	    if not source or not source.startswith("git+"):
	        return False
	    return normalize(re.split(r"[?#]", source[4:], maxsplit=1)[0]) == normalize(url)


	TAG = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
	GIT_KEY = re.compile(r'\bgit\s*=\s*"([^"]*)"')
	TAG_KEY = re.compile(r'\btag\s*=\s*"([^"]*)"')
	VERSION_KEY = re.compile(r'\bversion\s*=\s*"([^"]*)"')
	TAG_IN_TEXT = re.compile(r"\bv\d+\.\d+\.\d+\b")


	def pin_lines(manifests, url):
	    """Every manifest line naming url: (path, index, old tag)."""
	    found = []
	    for path, text in manifests.items():
	        for index, line in enumerate(text.split("\n")):
	            code = line.split("#", 1)[0]
	            match = GIT_KEY.search(code)
	            if not match or normalize(match.group(1)) != normalize(url):
	                continue
	            where = path + ":" + str(index + 1)
	            if "{" not in code or "}" not in code:
	                raise Refusal(where + ": the dependency on " + url + " is not a one-line inline "
	                              "table; write it as { git = ..., tag = ..., version = ... }")
	            if re.search(r"\b(rev|branch)\s*=", code):
	                raise Refusal(where + ": the dependency on " + url + " pins a rev or a branch, "
	                              "not a tag")
	            tag = TAG_KEY.search(code)
	            version = VERSION_KEY.search(code)
	            if not tag or not TAG.match(tag.group(1)):
	                raise Refusal(where + ": the dependency on " + url + " has no tag = \"vX.Y.Z\"")
	            if not version:
	                raise Refusal(where + ": the dependency on " + url + " has no version = \"X.Y\"")
	            found.append((path, index, tag.group(1)))
	    return found


	def rewrite_line(line, tag):
	    major, minor = TAG.match(tag).group(1, 2)
	    line = TAG_KEY.sub('tag = "' + tag + '"', line, count=1)
	    return VERSION_KEY.sub('version = "' + major + "." + minor + '"', line, count=1)


	def doc_line(path, text, marker, old):
	    lines = [index for index, line in enumerate(text.split("\n")) if marker in line]
	    if len(lines) != 1:
	        raise Refusal(path + ": holds " + str(len(lines)) + " lines marked " + marker
	                      + "; it must hold exactly one, the current pin")
	    index = lines[0]
	    tags = TAG_IN_TEXT.findall(text.split("\n")[index])
	    if len(tags) != 1:
	        raise Refusal(path + ":" + str(index + 1) + ": the line marked " + marker + " names "
	                      + str(len(tags)) + " tags; it must name exactly one")
	    if tags[0] != old:
	        raise Refusal(path + ":" + str(index + 1) + ": the current pin it names is " + tags[0]
	                      + ", but the manifests pin " + old)
	    return index


	# --- the two views of the graph -------------------------------------------------------------

	class Graph:
	    """Packages by (name, version, source), their edges keyed by dependency name, and the
	    workspace members."""

	    def __init__(self, packages, edges, members):
	        self.packages = packages  # key -> (name, version, source)
	        self.edges = edges  # key -> {dependency name: set of keys}
	        self.members = members  # set of keys

	    def closure(self, url):
	        roots = [pid for pid, (_, _, source) in self.packages.items() if from_url(source, url)]
	        seen = set(roots)
	        stack = list(roots)
	        while stack:
	            for targets in self.edges.get(stack.pop(), {}).values():
	                for target in targets:
	                    if target not in seen:
	                        seen.add(target)
	                        stack.append(target)
	        return seen


	def from_metadata(text):
	    # Keyed by (name, version, source), as Cargo.lock is: a git package's ID names its URL but
	    # not its commit, which only the source carries.
	    data = json.loads(text)
	    keys = {pkg["id"]: (pkg["name"], pkg["version"], pkg.get("source"))
	            for pkg in data["packages"]}
	    edges = {}
	    for node in (data.get("resolve") or {}).get("nodes", []):
	        named = {}
	        for dep in node.get("deps", []):
	            named.setdefault(dep["name"], set()).add(keys[dep["pkg"]])
	        edges[keys[node["id"]]] = named
	    members = {keys[pid] for pid in data.get("workspace_members", []) if pid in keys}
	    return Graph({key: key for key in keys.values()}, edges, members)


	def workspace_manifests(text):
	    """The root manifest and every workspace member's, relative to the current directory."""
	    data = json.loads(text)
	    here = os.path.realpath(os.getcwd())
	    manifests = {os.path.relpath(os.path.join(os.path.realpath(data["workspace_root"]),
	                                              "Cargo.toml"), here)}
	    members = set(data.get("workspace_members", []))
	    for pkg in data["packages"]:
	        if pkg["id"] in members:
	            manifests.add(os.path.relpath(os.path.realpath(pkg["manifest_path"]), here))
	    return manifests


	def parse_lock(text):
	    """Cargo.lock's [[package]] entries: name, version, source and dependency strings."""
	    entries = []
	    current = None
	    in_deps = False
	    for raw in text.split("\n"):
	        line = raw.strip()
	        if line == "[[package]]":
	            current = {"name": None, "version": None, "source": None, "dependencies": []}
	            entries.append(current)
	            in_deps = False
	            continue
	        if line.startswith("["):
	            current = None
	            in_deps = False
	            continue
	        if current is None:
	            continue
	        if in_deps:
	            if line.startswith("]"):
	                in_deps = False
	            else:
	                current["dependencies"].extend(re.findall(r'"([^"]*)"', line))
	            continue
	        match = re.match(r'^(name|version|source)\s*=\s*"(.*)"$', line)
	        if match:
	            current[match.group(1)] = match.group(2)
	            continue
	        match = re.match(r"^dependencies\s*=\s*\[(.*)$", line)
	        if match:
	            rest = match.group(1)
	            current["dependencies"].extend(re.findall(r'"([^"]*)"', rest))
	            in_deps = not rest.rstrip().endswith("]")
	    return entries


	def from_lock(text, member_keys):
	    entries = parse_lock(text)
	    keys = [(entry["name"], entry["version"], entry["source"]) for entry in entries]
	    packages = {key: key for key in keys}
	    edges = {}
	    for entry, key in zip(entries, keys):
	        named = {}
	        for dependency in entry["dependencies"]:
	            target = resolve_lock_dependency(dependency, keys, key)
	            named.setdefault(target[0], set()).add(target)
	        edges[key] = named
	    return Graph(packages, edges, {key for key in keys if key in member_keys})


	def resolve_lock_dependency(text, keys, owner):
	    match = re.match(r"^(\S+)(?: (\S+))?(?: \((.*)\))?$", text)
	    if not match:
	        raise Refusal("Cargo.lock: " + owner[0] + " " + owner[1] + " has a dependency \"" + text
	                      + "\" that does not parse")
	    name, version, source = match.groups()
	    candidates = [key for key in keys if key[0] == name
	                  and (version is None or key[1] == version)
	                  and (source is None or key[2] == source)]
	    if len(candidates) != 1:
	        raise Refusal("Cargo.lock: " + owner[0] + " " + owner[1] + "'s dependency \"" + text
	                      + "\" names " + str(len(candidates)) + " packages")
	    return candidates[0]


	def describe(graph, pid):
	    name, version, source = graph.packages.get(pid, (str(pid), "?", None))
	    return name + " " + version + " (" + (source or "path") + ")"


	def compare(label, before, after, url):
	    """The problems of one view: packages added or removed outside the closures, and edges
	    that changed target outside them."""
	    closures = before.closure(url) | after.closure(url)
	    problems = []
	    for pid in sorted(set(after.packages) - set(before.packages), key=str):
	        if pid not in closures:
	            problems.append(label + ": " + describe(after, pid) + " was added outside the "
	                            "rust-common closure")
	    for pid in sorted(set(before.packages) - set(after.packages), key=str):
	        if pid not in closures:
	            problems.append(label + ": " + describe(before, pid) + " was removed outside the "
	                            "rust-common closure")
	    for pid in sorted(set(before.packages) & set(after.packages), key=str):
	        old_edges = before.edges.get(pid, {})
	        new_edges = after.edges.get(pid, {})
	        for name in sorted(set(old_edges) | set(new_edges)):
	            old = old_edges.get(name, set())
	            new = new_edges.get(name, set())
	            if old == new:
	                continue
	            if pid in closures and old <= closures and new <= closures:
	                continue
	            if (pid in before.members and old and new
	                    and all(from_url(before.packages[target][2], url) for target in old)
	                    and all(from_url(after.packages[target][2], url) for target in new)):
	                continue
	            problems.append(label + ": the edge " + describe(before, pid) + " -> " + name
	                            + " moved from "
	                            + (", ".join(describe(before, t) for t in sorted(old, key=str))
	                               or "nothing")
	                            + " to "
	                            + (", ".join(describe(after, t) for t in sorted(new, key=str))
	                               or "nothing")
	                            + ", outside the rust-common closure")
	    return problems


	def report(before, after, url):
	    old_rc = {pid for pid, (_, _, source) in before.packages.items() if from_url(source, url)}
	    new_rc = {pid for pid, (_, _, source) in after.packages.items() if from_url(source, url)}
	    lines = []
	    for pid in sorted(old_rc - new_rc, key=str):
	        lines.append("  - " + describe(before, pid))
	    for pid in sorted(new_rc - old_rc, key=str):
	        lines.append("  + " + describe(after, pid))
	    others_removed = set(before.packages) - set(after.packages) - old_rc
	    others_added = set(after.packages) - set(before.packages) - new_rc
	    for pid in sorted(others_removed, key=str):
	        lines.append("  - " + describe(before, pid))
	    for pid in sorted(others_added, key=str):
	        lines.append("  + " + describe(after, pid))
	    closure = after.closure(url)
	    by_name = {}
	    for pid, (name, _, _) in after.packages.items():
	        by_name.setdefault(name, []).append(pid)
	    for name, pids in sorted(by_name.items()):
	        ours = [pid for pid in pids if pid in closure and pid not in new_rc]
	        theirs = [pid for pid in pids if pid not in closure]
	        if ours and theirs:
	            lines.append("  split: " + name + ": the consumer's "
	                         + ", ".join(describe(after, pid) for pid in sorted(theirs, key=str))
	                         + " beside rust-common's "
	                         + ", ".join(describe(after, pid) for pid in sorted(ours, key=str)))
	    return lines


	# --- the bump -------------------------------------------------------------------------------

	class Rollback:
	    """Copies the files into a snapshot directory, at their paths, on entry; on any exception,
	    a signal's included, copies every one back from it before the exception goes on. The
	    directory is named first, so a restore that cannot finish (or a kill) leaves it to copy from
	    by hand; it is removed once the files are back or the bump succeeded."""

	    def __init__(self, paths):
	        self.paths = paths
	        self.snapshot = None

	    def __enter__(self):
	        self.snapshot = tempfile.mkdtemp(prefix="rust-pin-bump.")
	        for path in self.paths:
	            target = os.path.join(self.snapshot, path)
	            os.makedirs(os.path.dirname(target), exist_ok=True)
	            shutil.copyfile(path, target)
	        return self

	    def __exit__(self, kind, _value, _trace):
	        if kind is not None:
	            say("restoring " + ", ".join(self.paths) + " from " + self.snapshot)
	            for path in self.paths:
	                shutil.copyfile(os.path.join(self.snapshot, path), path)
	            say("rolled back: every file restored")
	        shutil.rmtree(self.snapshot, ignore_errors=True)
	        return False


	def main():
	    parser = argparse.ArgumentParser(prog=PROG)
	    parser.add_argument("--tag", required=True)
	    parser.add_argument("--url", required=True)
	    parser.add_argument("--cargo", default="cargo")
	    parser.add_argument("--doc", action="append", default=[])
	    parser.add_argument("--marker", default="rust-pin-current")
	    args = parser.parse_args()
	    if not TAG.match(args.tag):
	        raise Refusal("TAG " + repr(args.tag) + " is not of the form vX.Y.Z")
	    if not args.url.strip():
	        raise Refusal("RUST_PIN_URL is empty")

	    tracked = [path for path in run(["git", "ls-files", "-z", "--", "Cargo.toml",
	                                     "*/Cargo.toml"]).split("\0") if path]
	    if not tracked:
	        raise Refusal("no tracked Cargo.toml")
	    if not run(["git", "ls-files", "--", "Cargo.lock"]).strip():
	        raise Refusal("Cargo.lock is not tracked")
	    for doc in args.doc:
	        if not run(["git", "ls-files", "--", doc]).strip():
	            raise Refusal(doc + " (RUST_PIN_DOCS) is not a tracked file")
	    dirty = run(["git", "status", "--porcelain", "--untracked-files=no", "--", "Cargo.lock"]
	                + tracked + args.doc).strip()
	    if dirty:
	        raise Refusal("these files have uncommitted changes; commit or stash them first:\n"
	                      + dirty)

	    cargo = args.cargo
	    before_text = run([cargo, "metadata", "--format-version", "1", "--locked", "--all-features"])
	    workspace = workspace_manifests(before_text)

	    texts = {}
	    for path in tracked + ["Cargo.lock"] + args.doc:
	        with open(path, encoding="utf-8", newline="") as handle:
	            texts[path] = handle.read()
	    manifests = {path: texts[path] for path in tracked}
	    pins = pin_lines(manifests, args.url)
	    if not pins:
	        raise Refusal("no tracked Cargo.toml line depends on " + args.url)
	    outside = sorted({path for path, _, _ in pins if path not in workspace})
	    if outside:
	        raise Refusal(", ".join(outside) + " pin " + args.url + " but are not this workspace's "
	                      "(their own lock is not re-resolved here); bump them on their own")
	    olds = sorted({tag for _, _, tag in pins})
	    if len(olds) != 1:
	        raise Refusal("the pins disagree (" + ", ".join(olds) + "); align them first")
	    old = olds[0]
	    if old == args.tag:
	        raise Refusal("the pin is already " + old)
	    doc_lines = {doc: doc_line(doc, texts[doc], args.marker, old) for doc in args.doc}

	    def on_signal(number, _frame):
	        raise Refusal("interrupted by signal " + str(number))

	    for number in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
	        signal.signal(number, on_signal)

	    with Rollback(list(texts)):
	        before = from_metadata(before_text)

	        expected = {}
	        for path in tracked:
	            lines = manifests[path].split("\n")
	            for pin_path, index, _ in pins:
	                if pin_path == path:
	                    lines[index] = rewrite_line(lines[index], args.tag)
	            expected[path] = "\n".join(lines)
	        for doc, index in doc_lines.items():
	            lines = texts[doc].split("\n")
	            lines[index] = lines[index].replace(old, args.tag)
	            expected[doc] = "\n".join(lines)
	        for path, text in expected.items():
	            if text != texts[path]:
	                with open(path, "w", encoding="utf-8", newline="") as handle:
	                    handle.write(text)

	        run([cargo, "metadata", "--format-version", "1", "--all-features"], capture=False)
	        after_text = run([cargo, "metadata", "--format-version", "1", "--locked",
	                          "--all-features"])
	        after = from_metadata(after_text)

	        problems = []
	        for path, text in expected.items():
	            with open(path, encoding="utf-8", newline="") as handle:
	                if handle.read() != text:
	                    problems.append(path + " changed beyond the pin lines")
	        with open("Cargo.lock", encoding="utf-8", newline="") as handle:
	            lock_after = handle.read()
	        sources = sorted({entry["source"] for entry in parse_lock(lock_after)
	                          if from_url(entry["source"], args.url)})
	        if len(sources) != 1 or "?tag=" + args.tag + "#" not in sources[0]:
	            problems.append("Cargo.lock holds " + str(len(sources)) + " revisions of "
	                            + args.url + " (" + ", ".join(sources) + "); want exactly one, at "
	                            + args.tag)
	        problems += compare("metadata", before, after, args.url)
	        member_keys = before.members
	        lock_before = from_lock(texts["Cargo.lock"], member_keys)
	        lock_graph = from_lock(lock_after, member_keys)
	        problems += compare("Cargo.lock", lock_before, lock_graph, args.url)
	        if problems:
	            raise Refusal("the re-resolution moved more than the pin:\n  "
	                          + "\n  ".join(problems))

	    print(PROG + ": " + old + " -> " + args.tag + " in " + str(len(pins)) + " manifest line(s)"
	          + (" and " + ", ".join(args.doc) if args.doc else ""))
	    print(PROG + ": what moved in Cargo.lock:")
	    for line in report(lock_before, lock_graph, args.url) or ["  nothing"]:
	        print(line)


	try:
	    main()
	except Refusal as refusal:
	    say(str(refusal))
	    sys.exit(1)
endef

# Exported only to a run that asks for rust-pin-bump, so no other recipe carries the script.
ifneq ($(filter rust-pin-bump,$(MAKECMDGOALS)),)
export _RUST_PIN_BUMP_SCRIPT = $(value _RUST_PIN_BUMP_PY)
endif

.PHONY: rust-pin-bump
rust-pin-bump: ## Move the RUST_PIN_URL pins to TAG=vX.Y.Z, re-resolve Cargo.lock, check that only rust-common moved
	@test -n "$(strip $(RUST_PIN_URL))" || { echo "rust-pin-bump: set RUST_PIN_URL to the git URL whose pins move" >&2; exit 1; }
	@test -n "$(strip $(TAG))" || { echo "rust-pin-bump: set TAG=vX.Y.Z" >&2; exit 1; }
	@command -v "$(RUST_PIN_PYTHON)" >/dev/null 2>&1 || { echo "rust-pin-bump needs Python 3: $(RUST_PIN_PYTHON) not found" >&2; exit 1; }
	@test -n "$$_RUST_PIN_BUMP_SCRIPT" || { echo "rust-pin-bump: run it as a goal of its own (make rust-pin-bump TAG=...), not as a prerequisite" >&2; exit 1; }
	cd "$(or $(REPO_ROOT),.)" && "$(RUST_PIN_PYTHON)" -c "$(_RUST_PIN_BUMP_RUN)" --tag "$(strip $(TAG))" --url "$(strip $(RUST_PIN_URL))" --cargo "$(CARGO)" --marker "$(RUST_PIN_DOC_MARKER)"$(if $(strip $(RUST_PIN_DOCS)), $(foreach doc,$(strip $(RUST_PIN_DOCS)),--doc "$(doc)"))

.PHONY: verify-rust
verify-rust: rust-build rust-fmt-check rust-clippy rust-test rust-doc ## Verify the Rust code (build, fmt, clippy, tests, docs)

endif  # MK_COMMON_RUST_INCLUDED
