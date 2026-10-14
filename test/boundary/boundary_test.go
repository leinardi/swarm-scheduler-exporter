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

// Package boundary_test pins the lint rule that keeps the exporter read-only against Docker: the
// docker-sdk-boundary depguard rule, which confines the Docker SDK to internal/collector and
// cmd/swarm-scheduler-exporter (and the integration harness). A rule that stops matching fails
// nothing on its own -- the linter just goes quiet -- so this test reads the rule out of
// .golangci.yaml, and runs golangci-lint with it on a scratch module.
package boundary_test

import (
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"testing"

	"go.yaml.in/yaml/v3"

	"github.com/leinardi/swarm-scheduler-exporter/test/internal/toolgate"
)

const ruleName = "docker-sdk-boundary"

// wantDeny are the SDK prefixes the rule denies. The trailing slash is load-bearing: depguard
// matches a plain string prefix, and without it github.com/moby/moby would also deny moby/term.
var wantDeny = []string{"github.com/docker/docker/", "github.com/moby/moby/"}

// wantFiles is the exact scope: everywhere except the two exporter packages that may reach Docker
// and the integration harness and suite.
var wantFiles = []string{
	"!**/internal/collector/**.go",
	"!**/cmd/swarm-scheduler-exporter/**.go",
	"!**/internal/testenv/**.go",
	"!**/test/integration/**.go",
}

type golangciConfig struct {
	Linters struct {
		Default  string   `yaml:"default"`
		Disable  []string `yaml:"disable"`
		Settings struct {
			Depguard struct {
				Rules map[string]struct {
					Files []string `yaml:"files"`
					Allow []string `yaml:"allow"`
					Deny  []struct {
						Pkg  string `yaml:"pkg"`
						Desc string `yaml:"desc"`
					} `yaml:"deny"`
				} `yaml:"rules"`
			} `yaml:"depguard"`
		} `yaml:"settings"`
		Exclusions struct {
			Generated string `yaml:"generated"`
		} `yaml:"exclusions"`
	} `yaml:"linters"`
}

// TestBoundaryRuleIsConfigured pins that depguard runs and that the rule denies the SDK everywhere
// but its owners, with no allow list that could exempt part of it unseen.
func TestBoundaryRuleIsConfigured(t *testing.T) {
	t.Parallel()

	var config golangciConfig

	err := yaml.Unmarshal(readConfig(t), &config)
	if err != nil {
		t.Fatalf("parse .golangci.yaml: %v", err)
	}

	if config.Linters.Default != "all" || slices.Contains(config.Linters.Disable, "depguard") {
		t.Errorf("depguard does not run (default %q, disable %v)", config.Linters.Default,
			config.Linters.Disable)
	}

	if config.Linters.Exclusions.Generated != "strict" {
		t.Errorf("exclusions.generated is %q, want strict: lax skips any file that merely "+
			"mentions generated code", config.Linters.Exclusions.Generated)
	}

	rule, ok := config.Linters.Settings.Depguard.Rules[ruleName]
	if !ok {
		t.Fatalf("depguard rule %q is missing", ruleName)
	}

	if !slices.Equal(rule.Files, wantFiles) {
		t.Errorf("rule %s applies to %q, want exactly %q", ruleName, rule.Files, wantFiles)
	}

	if len(rule.Allow) != 0 {
		t.Errorf("rule %s has an allow list %q", ruleName, rule.Allow)
	}

	deny := make([]string, 0, len(rule.Deny))

	for _, entry := range rule.Deny {
		deny = append(deny, entry.Pkg)

		if strings.TrimSpace(entry.Desc) == "" {
			t.Errorf("rule %s: deny entry %q has no desc", ruleName, entry.Pkg)
		}
	}

	slices.Sort(deny)

	if !slices.Equal(deny, wantDeny) {
		t.Errorf("rule %s denies %q, want exactly %q", ruleName, deny, wantDeny)
	}
}

// TestLinterEnforcesTheBoundary runs golangci-lint with this repository's configuration on a
// scratch copy of the module: an SDK import outside the owners must be reported, and the same
// import in internal/collector must not.
func TestLinterEnforcesTheBoundary(t *testing.T) {
	t.Parallel()

	linter := toolgate.Lookup(t, toolgate.GolangciLint)
	repo := toolgate.RepoRoot(t)

	dir := t.TempDir()

	root, err := os.OpenRoot(dir)
	if err != nil {
		t.Fatalf("open the scratch module: %v", err)
	}

	t.Cleanup(func() { _ = root.Close() })

	// The module's own go.mod and go.sum, so the SDK resolves to the version the exporter uses.
	for _, name := range []string{"go.mod", "go.sum"} {
		data, readErr := os.ReadFile(filepath.Join(repo, name))
		if readErr != nil {
			t.Fatalf("read %s: %v", name, readErr)
		}

		writeFile(t, root, name, string(data))
	}

	writeFile(t, root, ".golangci.yaml", string(readConfig(t)))
	writeFile(t, root, filepath.Join("internal", "labels", "x.go"), scratchSource("labels"))
	writeFile(t, root, filepath.Join("internal", "collector", "y.go"), scratchSource("collector"))

	// The configuration as written decides which linters run: an --enable-only here would turn a
	// disabled depguard back on and hide exactly the regression this test is for.
	cmd := exec.CommandContext(
		t.Context(),
		linter,
		"run",
		"--max-issues-per-linter=0",
		"--max-same-issues=0",
		"--uniq-by-line=false",
		"./...",
	)
	cmd.Dir = dir

	cmd.Env = append(os.Environ(), "GOFLAGS=-mod=mod")

	out, _ := cmd.CombinedOutput()
	output := string(out)

	want := "import 'github.com/moby/moby/client' is not allowed from list '" + ruleName + "'"

	found := false

	for line := range strings.SplitSeq(output, "\n") {
		if !strings.Contains(line, want) {
			continue
		}

		switch {
		case strings.Contains(line, filepath.Join("internal", "labels", "x.go")):
			found = true
		case strings.Contains(line, filepath.Join("internal", "collector", "y.go")):
			t.Errorf("the rule fired in internal/collector, which owns the SDK: %s", line)
		}
	}

	if !found {
		t.Errorf("golangci-lint did not report the SDK import in internal/labels:\n%s", output)
	}
}

func scratchSource(pkg string) string {
	return "package " + pkg + `

import "github.com/moby/moby/client"

// Scratch reaches the Docker SDK.
var Scratch = client.FromEnv
`
}

func readConfig(t *testing.T) []byte {
	t.Helper()

	data, err := os.ReadFile(filepath.Join(toolgate.RepoRoot(t), ".golangci.yaml"))
	if err != nil {
		t.Fatalf("read .golangci.yaml: %v", err)
	}

	return data
}

func writeFile(t *testing.T, root *os.Root, name, content string) {
	t.Helper()

	err := root.MkdirAll(filepath.Dir(name), 0o750)
	if err != nil {
		t.Fatalf("create %s: %v", filepath.Dir(name), err)
	}

	err = root.WriteFile(name, []byte(content), 0o600)
	if err != nil {
		t.Fatalf("write %s: %v", name, err)
	}
}
