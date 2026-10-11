/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to deal
 * in the Software without restriction, including without limitation the rights
 * to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
 * copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in all
 * copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
 * OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
 * SOFTWARE.
 */

package dockerfiles_test

import (
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"

	"github.com/leinardi/swarm-scheduler-exporter/test/dockerfiles"
)

const (
	shippedDir        = "deployments/docker"
	shippedDockerfile = "deployments/docker/Dockerfile"
	workflowsDir      = ".github/workflows"
	wantPlatforms     = "linux/amd64,linux/arm64"
	runtimeUser       = "65532:65532"
	binary            = "/usr/local/bin/swarm-scheduler-exporter"
	// healthcheck is the runtime stage's HEALTHCHECK, timings included, as the Dockerfile's comment
	// above it explains them.
	healthcheck = `--interval=30s --timeout=5s --start-period=90s --retries=3 CMD ["` + binary +
		`", "-healthcheck"]`
)

// TestShippedDockerfileIsTheOnlyOne pins that deployments/docker holds exactly the image that
// ships. A second file there would look shipped to every reader and be built by nothing.
func TestShippedDockerfileIsTheOnlyOne(t *testing.T) {
	t.Parallel()

	entries, err := os.ReadDir(repoPath(t, shippedDir))
	if err != nil {
		t.Fatalf("read %s: %v", shippedDir, err)
	}

	var found []string

	for _, entry := range entries {
		if isDockerfile(entry.Name()) {
			found = append(found, entry.Name())
		}
	}

	if !slices.Equal(found, []string{"Dockerfile"}) {
		t.Errorf("%s holds %v, want exactly [Dockerfile]", shippedDir, found)
	}

	if all := allDockerfiles(t); len(all) != 1 {
		t.Errorf("the repository holds the Dockerfiles %v, want only %s", all, shippedDockerfile)
	}
}

// TestSyntaxDirectiveIsPinned pins the BuildKit frontend that reads the Dockerfile. A bare
// docker/dockerfile:1.7 is a tag that can be moved under a build, and Dependabot never updates the
// line, so it is a literal reference pinned by digest (docs/release.md).
func TestSyntaxDirectiveIsPinned(t *testing.T) {
	t.Parallel()

	syntax, ok := dockerfiles.Syntax(readRepoFile(t, shippedDockerfile))
	if !ok {
		t.Fatalf("%s has no syntax parser directive on its first line", shippedDockerfile)
	}

	if dockerfiles.Repository(syntax) != "docker/dockerfile" {
		t.Errorf("%s reads with the frontend %q, want docker/dockerfile", shippedDockerfile, syntax)
	}

	if !dockerfiles.DigestPinned(syntax) {
		t.Errorf("%s reads with the frontend %q, which is not pinned by digest", shippedDockerfile,
			syntax)
	}
}

// TestShippedStages pins the two stages, in order, each from a literal base pinned by tag and not
// by digest: dhi.io republishes its tags with security fixes, and a rebuild should pick those up
// (docs/release.md). A variable or latest would let a build change base unseen.
func TestShippedStages(t *testing.T) {
	t.Parallel()

	stages := shippedStages(t)

	names := make([]string, 0, len(stages))
	for _, stage := range stages {
		names = append(names, stage.Name)

		if !dockerfiles.TagPinned(stage.Image) {
			t.Errorf("stage %q is FROM %q, which is not a literal reference pinned by tag alone",
				stage.Name, stage.Image)
		}
	}

	if !slices.Equal(names, []string{"build", ""}) {
		t.Errorf("the stages are %q, want build, then the unnamed runtime stage", names)
	}
}

// TestBuildStage pins where the binary is compiled: on the build platform, since Go cross-compiles,
// from the golang image. Its tag follows go.mod, which test/toolchain holds.
func TestBuildStage(t *testing.T) {
	t.Parallel()

	build := shippedStages(t)[0]

	if build.Platform != "$BUILDPLATFORM" {
		t.Errorf("the build stage runs on --platform=%q, want $BUILDPLATFORM", build.Platform)
	}

	if got := dockerfiles.Repository(build.Image); got != "dhi.io/golang" {
		t.Errorf("the build stage is FROM %s, want dhi.io/golang", got)
	}

	instructions := joined(build)
	for _, want := range []string{
		"ARG VERSION", "ARG COMMIT", "ARG DATE", "ARG TARGETOS", "ARG TARGETARCH",
		"CGO_ENABLED=0", "-trimpath", "-buildvcs=false",
		"-o /out/swarm-scheduler-exporter ./cmd/swarm-scheduler-exporter",
	} {
		if !strings.Contains(instructions, want) {
			t.Errorf("the build stage lacks %q:\n%s", want, instructions)
		}
	}
}

// TestRuntimeStage pins the posture of the image that ships: no shell to run anything in, nothing
// to write to, an unprivileged numeric user, the healthcheck in exec form, and the seven OCI labels.
func TestRuntimeStage(t *testing.T) {
	t.Parallel()

	runtime := shippedStages(t)[1]

	if got := dockerfiles.Repository(runtime.Image); got != "dhi.io/static" {
		t.Errorf("the runtime stage is FROM %s, want dhi.io/static", got)
	}

	if runtime.Platform != "" {
		t.Errorf("the runtime stage pins --platform=%s; it has to follow the target platform",
			runtime.Platform)
	}

	want := map[string]string{
		"USER":        runtimeUser,
		"EXPOSE":      "8888",
		"HEALTHCHECK": healthcheck,
		"ENTRYPOINT":  `["` + binary + `"]`,
		"COPY":        "--from=build /out/swarm-scheduler-exporter " + binary,
	}

	seen := map[string][]string{}

	for _, instruction := range runtime.Instructions {
		switch instruction.Keyword {
		case "RUN", "SHELL", "VOLUME", "WORKDIR", "ADD", "ENV":
			t.Errorf("the runtime stage carries %s %s; it has no shell and writes nothing",
				instruction.Keyword, instruction.Args)
		}

		seen[instruction.Keyword] = append(seen[instruction.Keyword], instruction.Args)
	}

	for keyword, args := range want {
		if !slices.Equal(seen[keyword], []string{args}) {
			t.Errorf("the runtime stage has %s %q, want exactly %s %s", keyword, seen[keyword],
				keyword, args)
		}
	}

	labels := dockerfiles.Labels(runtime)
	wantLabels := map[string]string{
		"org.opencontainers.image.title":       "swarm-scheduler-exporter",
		"org.opencontainers.image.description": labels["org.opencontainers.image.description"],
		"org.opencontainers.image.source":      "https://github.com/leinardi/swarm-scheduler-exporter",
		"org.opencontainers.image.version":     "${VERSION}",
		"org.opencontainers.image.revision":    "${COMMIT}",
		"org.opencontainers.image.created":     "${DATE}",
		"org.opencontainers.image.licenses":    "MIT",
	}

	if labels["org.opencontainers.image.description"] == "" {
		t.Error("the runtime stage has no org.opencontainers.image.description")
	}

	for key, value := range wantLabels {
		if labels[key] != value {
			t.Errorf("label %s is %q, want %q", key, labels[key], value)
		}
	}

	if len(labels) != len(wantLabels) {
		t.Errorf("the runtime stage carries labels %v, want exactly the seven OCI labels", labels)
	}
}

// TestVersionIsStamped pins where the version comes from: package main's version, commit and date,
// in the image and in make go-build alike. A path that stamped nothing would ship a binary
// reporting "dev".
func TestVersionIsStamped(t *testing.T) {
	t.Parallel()

	for _, path := range []string{shippedDockerfile, ".mk/go.mk"} {
		content := readRepoFile(t, path)

		for _, field := range []string{"version", "commit", "date"} {
			if !strings.Contains(content, "-X main."+field+"=") &&
				!strings.Contains(content, "-X 'main."+field+"=") {
				t.Errorf("%s does not stamp main.%s", path, field)
			}
		}
	}
}

// TestPlatforms pins the two platforms wherever a workflow names platforms: dhi.io/static has no
// arm/v7.
func TestPlatforms(t *testing.T) {
	t.Parallel()

	declaration := regexp.MustCompile(`(?mi)^\s*(?:-\s*)?(platforms?|PLATFORMS)\s*:\s*(.+)$`)
	declared := 0

	for _, workflow := range workflows(t) {
		content := readRepoFile(t, workflow)

		for _, match := range declaration.FindAllStringSubmatch(content, -1) {
			value := strings.Trim(strings.TrimSpace(match[2]), `"'`)
			if strings.HasPrefix(value, "${{") {
				continue
			}

			declared++

			if value != wantPlatforms {
				t.Errorf("%s declares %s %q, want %q", workflow, match[1], value, wantPlatforms)
			}
		}

		if strings.Contains(content, "linux/arm/v") || strings.Contains(content, "linux/386") {
			t.Errorf("%s names a platform other than %s", workflow, wantPlatforms)
		}
	}

	if declared == 0 {
		t.Error("no workflow declares its platforms; the check would pass on nothing")
	}
}

// TestParseRejectsWhatTheAssertionsMustNotMiss is the negative control for the reader: each of
// these inputs has to come out as something the assertions above refuse.
func TestParseRejectsWhatTheAssertionsMustNotMiss(t *testing.T) {
	t.Parallel()

	const digest = "@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"

	for name, image := range map[string]string{
		"a tag only":              "dhi.io/static:20250419",
		"a variable tag":          "dhi.io/golang:${GO_LINE}-alpine3.23-dev" + digest,
		"a variable image":        "$BASE" + digest,
		"a short digest":          "dhi.io/static:20250419@sha256:0123",
		"a digest with no tag":    "dhi.io/static" + digest,
		"a stage name, not a ref": "build",
	} {
		if dockerfiles.DigestPinned(image) {
			t.Errorf("%s (%q) passes as digest-pinned", name, image)
		}
	}

	if !dockerfiles.DigestPinned("dhi.io/static:20250419" + digest) {
		t.Error("a literal tag plus digest does not pass as digest-pinned")
	}

	for name, image := range map[string]string{
		"no tag":                  "dhi.io/static",
		"latest":                  "dhi.io/static:latest",
		"a variable tag":          "dhi.io/golang:${GO_LINE}-alpine3.23-dev",
		"a variable image":        "$BASE",
		"a tag and a digest":      "dhi.io/static:20250419" + digest,
		"a stage name, not a ref": "build",
	} {
		if dockerfiles.TagPinned(image) {
			t.Errorf("%s (%q) passes as pinned by tag alone", name, image)
		}
	}

	if !dockerfiles.TagPinned("dhi.io/golang:1.26.9-alpine3.23-dev") {
		t.Error("a literal tag does not pass as pinned by tag")
	}

	stages, err := dockerfiles.Parse(
		"FROM a:1 AS one\nRUN echo \\\n  two\n# note\nFROM b:2\nUSER 1:1\n",
	)
	if err != nil {
		t.Fatalf("parse: %v", err)
	}

	if len(stages) != 2 || stages[0].Name != "one" || stages[1].Name != "" ||
		joined(stages[0]) != "RUN echo two" || joined(stages[1]) != "USER 1:1" {
		t.Errorf("parsed %+v", stages)
	}

	for name, content := range map[string]string{
		"no directive":              "FROM a:1\n",
		"after a comment":           "# note\n# syntax=docker/dockerfile:1.7" + digest + "\n",
		"after a blank line":        "\n# syntax=docker/dockerfile:1.7" + digest + "\n",
		"after an instruction":      "FROM a:1\n# syntax=docker/dockerfile:1.7" + digest + "\n",
		"a directive with no value": "# syntax=\n",
	} {
		if syntax, ok := dockerfiles.Syntax(content); ok {
			t.Errorf("%s: read the syntax directive %q, which BuildKit ignores", name, syntax)
		}
	}

	syntax, ok := dockerfiles.Syntax(
		"# check=error=true\n#Syntax = docker/dockerfile:1.7" + digest + "\n",
	)
	if !ok || syntax != "docker/dockerfile:1.7"+digest {
		t.Errorf("a directive after another directive read as %q, %v", syntax, ok)
	}

	_, err = dockerfiles.Parse("FROM a:1\nRUN echo \\\n")
	if err == nil {
		t.Error("a file ending inside a continuation parsed")
	}
}

func shippedStages(t *testing.T) []dockerfiles.Stage {
	t.Helper()

	stages, err := dockerfiles.Parse(readRepoFile(t, shippedDockerfile))
	if err != nil {
		t.Fatalf("%s: %v", shippedDockerfile, err)
	}

	if len(stages) != 2 {
		t.Fatalf("%s has %d stages, want build and runtime", shippedDockerfile, len(stages))
	}

	return stages
}

func joined(stage dockerfiles.Stage) string {
	lines := make([]string, 0, len(stage.Instructions))
	for _, instruction := range stage.Instructions {
		lines = append(lines, instruction.Keyword+" "+instruction.Args)
	}

	return strings.Join(lines, "\n")
}

func isDockerfile(name string) bool {
	return name == "Dockerfile" || strings.HasSuffix(name, ".Dockerfile")
}

// allDockerfiles walks the repository for Dockerfiles, skipping installed and generated trees.
// Walking rather than asking git keeps a new file in the test cache key.
func allDockerfiles(t *testing.T) []string {
	t.Helper()

	root, err := os.OpenRoot(repoPath(t, "."))
	if err != nil {
		t.Fatalf("open the repository: %v", err)
	}

	defer root.Close()

	var found []string

	err = fs.WalkDir(root.FS(), ".", func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}

		if entry.IsDir() {
			switch entry.Name() {
			case ".git", "dist", ".cache", "vendor":
				return fs.SkipDir
			}

			return nil
		}

		if isDockerfile(entry.Name()) {
			found = append(found, path)
		}

		return nil
	})
	if err != nil {
		t.Fatalf("walk the repository: %v", err)
	}

	if !slices.Contains(found, shippedDockerfile) {
		t.Fatalf("the walk did not find %s, so it would find nothing else either",
			shippedDockerfile)
	}

	return found
}

// workflows lists the workflow files.
func workflows(t *testing.T) []string {
	t.Helper()

	entries, err := os.ReadDir(repoPath(t, workflowsDir))
	if err != nil {
		t.Fatalf("read %s: %v", workflowsDir, err)
	}

	var found []string

	for _, entry := range entries {
		if strings.HasSuffix(entry.Name(), ".yaml") || strings.HasSuffix(entry.Name(), ".yml") {
			found = append(found, workflowsDir+"/"+entry.Name())
		}
	}

	return found
}

func repoPath(t *testing.T, rel string) string {
	t.Helper()

	path, err := filepath.Abs(filepath.Join("..", "..", filepath.FromSlash(rel)))
	if err != nil {
		t.Fatalf("resolve %s: %v", rel, err)
	}

	return path
}

func readRepoFile(t *testing.T, rel string) string {
	t.Helper()

	data, err := os.ReadFile(repoPath(t, rel))
	if err != nil {
		t.Fatalf("read %s: %v", rel, err)
	}

	return string(data)
}
