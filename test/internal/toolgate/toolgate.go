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

// Package toolgate finds the external tools that the behavioral meta-tests drive.
//
// A behavioral test that skips when its tool is missing passes in exactly the place it is meant to
// guard: CI. So a missing tool is a skip on a developer machine and a failure wherever
// SSE_REQUIRE_TOOLS=1 is set, which the repo-gates CI job does. The tools are a closed list:
// Docker is deliberately not on it, because every Docker-dependent test sits behind the
// integration build tag and never runs untagged.
package toolgate

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"testing"
)

// RequireEnv is the variable that turns a missing tool from a skip into a failure.
const RequireEnv = "SSE_REQUIRE_TOOLS"

// GolangciLint is the one tool a behavioral meta-test may gate on.
const GolangciLint = "golangci-lint"

// Tools is the closed list of tools a meta-test may be gated on.
var Tools = []string{GolangciLint}

// Lookup returns the path of tool, or skips the test when it is missing locally and fails it when
// RequireEnv is set to 1. A tool outside Tools fails the test: widening the list is a decision, not
// a side effect of a new test.
func Lookup(t *testing.T, tool string) string {
	t.Helper()

	if !slices.Contains(Tools, tool) {
		t.Fatalf("toolgate: %q is not one of the gated tools %v", tool, Tools)
	}

	path, err := exec.LookPath(tool)
	if err == nil {
		return path
	}

	err = fmt.Errorf("look up %s on PATH: %w", tool, err)

	if os.Getenv(RequireEnv) == "1" {
		t.Fatalf("%s is required (%s=1) but was not found: %v", tool, RequireEnv, err)
	}

	t.Skipf("%s not found (%v); set %s=1 to make this a failure", tool, err, RequireEnv)

	return ""
}

// RepoRoot returns the repository root, found as the nearest ancestor of the working directory
// that holds go.mod.
func RepoRoot(t *testing.T) string {
	t.Helper()

	dir, err := os.Getwd()
	if err != nil {
		t.Fatalf("toolgate: working directory: %v", err)
	}

	for {
		_, statErr := os.Stat(filepath.Join(dir, "go.mod"))
		if statErr == nil {
			return dir
		}

		parent := filepath.Dir(dir)
		if parent == dir {
			t.Fatal("toolgate: no go.mod above the working directory")
		}

		dir = parent
	}
}
