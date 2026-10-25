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

// Package dockerfiles_test holds the image checks that are about the Go build: the build stage and
// the version stamps. The rest of the image's invariants are meta crate tests
// (meta/tests/dockerfiles.rs).
package dockerfiles_test

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/leinardi/swarm-scheduler-exporter/test/dockerfiles"
)

const shippedDockerfile = "deployments/docker/Dockerfile"

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
