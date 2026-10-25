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

package toolchain_test

import (
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"slices"
	"strings"
	"testing"

	"go.yaml.in/yaml/v3"

	"github.com/leinardi/swarm-scheduler-exporter/test/internal/toolgate"
)

// pinSource is one place a version is written: a file and the pattern whose first group is the
// version there.
type pinSource struct {
	file    string
	pattern *regexp.Regexp
}

var exactTag = regexp.MustCompile(`^v\d+\.\d+\.\d+$`)

// The actions whose inputs carry a shared pin.
const (
	setupGo       = "actions/setup-go"
	golangciLintA = "golangci/golangci-lint-action"
	rustToolchain = "dtolnay/rust-toolchain"
)

// TestActionStepsArePinned: no setup-go or golangci-lint-action step under .github leaves its
// version to the action's default, a range or a file the other pins do not see.
func TestActionStepsArePinned(t *testing.T) {
	t.Parallel()

	_, problems := actionPins(githubActionSteps(t))
	for _, problem := range problems {
		t.Error(problem)
	}
}

// TestGoLineIsOneValue: the go directive and the build stage's golang image name the same Go
// version, patch included -- the shipped binary is built with the toolchain CI tests and scans --
// and every setup-go step reads it from go.mod instead of writing it (TestActionStepsArePinned).
func TestGoLineIsOneValue(t *testing.T) {
	t.Parallel()

	expectOneValue(t, "Go version", []pinSource{
		{file: "go.mod", pattern: regexp.MustCompile(`(?m)^go (\d+\.\d+\.\d+)$`)},
		{
			file:    "deployments/docker/Dockerfile",
			pattern: regexp.MustCompile(`dhi\.io/golang:(\d+\.\d+\.\d+)-`),
		},
	}, nil)

	if strings.Contains(readRepoFile(t, "go.mod"), "\ntoolchain ") {
		t.Error("go.mod has a toolchain directive, a second Go version the image does not follow")
	}
}

// TestGolangciLintIsOneVersion: every golangci-lint-action step installs, by an exact version, the
// golangci-lint the pre-commit hook pins.
func TestGolangciLintIsOneVersion(t *testing.T) {
	t.Parallel()

	steps, _ := actionPins(githubActionSteps(t))
	if len(steps[golangciLintA]) == 0 {
		t.Error("no workflow installs golangci-lint; the repo-gates job needs it")
	}

	expectOneValue(t, "golangci-lint", []pinSource{
		{
			file: ".pre-commit-config.yaml",
			pattern: regexp.MustCompile(
				`repo: https://github\.com/golangci/golangci-lint\s+rev:\s*(v[\d.]+)`,
			),
		},
	}, steps[golangciLintA])
}

// TestHadolintIsOneVersion: the hadolint hook runs the system binary, so the version CI installs
// is the one that lints; it must follow the hook's rev, which Dependabot moves.
func TestHadolintIsOneVersion(t *testing.T) {
	t.Parallel()

	expectOneValue(t, "hadolint", []pinSource{
		{
			file: ".pre-commit-config.yaml",
			pattern: regexp.MustCompile(
				`repo: https://github\.com/hadolint/hadolint\s+rev:\s*(v[\d.]+)`,
			),
		},
		{
			file:    ".github/actions/setup-hadolint/action.yaml",
			pattern: regexp.MustCompile(`HADOLINT_VERSION:\s*["']?(v[\d.]+)`),
		},
	}, nil)
}

// TestRustToolchainIsOneValue: every dtolnay/rust-toolchain step installs the toolchain
// rust-toolchain.toml pins, so CI lints and tests with the compiler the developers use.
func TestRustToolchainIsOneValue(t *testing.T) {
	t.Parallel()

	var installs []pinValue

	for _, step := range githubActionSteps(t) {
		if step.action != rustToolchain {
			continue
		}

		installs = append(installs, pinValue{step.with["toolchain"], step.file})
	}

	if len(installs) == 0 {
		t.Error("no workflow installs the Rust toolchain; the rust job needs it")
	}

	expectOneValue(t, "Rust toolchain", []pinSource{
		{
			file:    "rust-toolchain.toml",
			pattern: regexp.MustCompile(`(?m)^channel = "(\d+\.\d+\.\d+)"$`),
		},
	}, installs)
}

// TestTrivyIsOneVersion: the release scan and the scheduled scan of the published image use one
// Trivy, so a finding means the same in both (docs/release.md).
func TestTrivyIsOneVersion(t *testing.T) {
	t.Parallel()

	trivy := regexp.MustCompile(`(?m)^\s*TRIVY_VERSION:\s*["']?(v\d+\.\d+\.\d+)`)

	expectOneValue(t, "Trivy", []pinSource{
		{file: ".github/workflows/release.yaml", pattern: trivy},
		{file: ".github/workflows/scheduled-scan.yaml", pattern: trivy},
	}, nil)
}

// TestActionPinRules is the negative control of the action rules: pinned steps give their values,
// and each unpinned form is a problem.
func TestActionPinRules(t *testing.T) {
	t.Parallel()

	steps, err := parseActionSteps("sample.yaml", []byte(`jobs:
  lint:
    steps:
      - uses: actions/setup-go@0123456789abcdef0123456789abcdef01234567 # v7.0.0
        with:
          go-version-file: go.mod
      - uses: golangci/golangci-lint-action@0123456789abcdef0123456789abcdef01234567 # v9.3.0
        with:
          install-only: true
          version: v2.14.0
`))
	if err != nil {
		t.Fatal(err)
	}

	values, problems := actionPins(steps)
	if len(problems) != 0 ||
		!slices.Equal(values[golangciLintA], []pinValue{{"v2.14.0", "sample.yaml"}}) {
		t.Errorf("pinned steps: %v, %v", values, problems)
	}

	steps, err = parseActionSteps("action.yml", []byte(`runs:
  using: composite
  steps:
    - uses: actions/setup-go@0123456789abcdef0123456789abcdef01234567
      with:
        go-version: "1.26"
    - uses: actions/setup-go@0123456789abcdef0123456789abcdef01234567
    - uses: actions/setup-go@0123456789abcdef0123456789abcdef01234567
      with:
        go-version-file: .go-version
    - uses: golangci/golangci-lint-action@0123456789abcdef0123456789abcdef01234567
      with:
        install-only: true
    - uses: golangci/golangci-lint-action@0123456789abcdef0123456789abcdef01234567
      with:
        version: latest
    - uses: some/other-action@0123456789abcdef0123456789abcdef01234567
      with:
        go-version: "1.27"
`))
	if err != nil {
		t.Fatal(err)
	}

	// The first setup-go breaks both rules (it writes a go-version and reads no go.mod).
	values, problems = actionPins(steps)
	if len(problems) != 7 || len(values) != 0 {
		t.Errorf("unpinned steps: want 7 problems and no values, got %v and %q", values, problems)
	}

	found := map[string][]string{}
	if expectOneValueIn(found, []pinValue{{"v1", "a"}, {"v2", "b"}}) == "" {
		t.Error("two different values were taken for one")
	}
}

// actionStep is one step, in a workflow or a composite action, that uses an action.
type actionStep struct {
	file string
	// action is the uses: value without its @ref.
	action string
	with   map[string]string
}

// pinValue is one version found and where.
type pinValue struct {
	value string
	file  string
}

// githubActionSteps parses every YAML file under .github for steps that use an action.
func githubActionSteps(t *testing.T) []actionStep {
	t.Helper()

	var steps []actionStep

	for _, file := range githubYAML(t) {
		parsed, err := parseActionSteps(file, []byte(readRepoFile(t, file)))
		if err != nil {
			t.Fatal(err)
		}

		steps = append(steps, parsed...)
	}

	return steps
}

// parseActionSteps finds every mapping with a uses: key anywhere in the document, so job steps and
// composite action steps alike.
func parseActionSteps(file string, content []byte) ([]actionStep, error) {
	var document any

	err := yaml.Unmarshal(content, &document)
	if err != nil {
		return nil, fmt.Errorf("parse %s: %w", file, err)
	}

	var steps []actionStep

	var walk func(node any)

	walk = func(node any) {
		switch value := node.(type) {
		case map[string]any:
			if uses, ok := value["uses"].(string); ok {
				step := actionStep{
					file:   file,
					action: strings.SplitN(uses, "@", 2)[0],
					with:   map[string]string{},
				}

				inputs, _ := value["with"].(map[string]any)
				for key, input := range inputs {
					step.with[key] = fmt.Sprint(input)
				}

				steps = append(steps, step)
			}

			for _, child := range value {
				walk(child)
			}
		case []any:
			for _, child := range value {
				walk(child)
			}
		}
	}

	walk(document)

	return steps, nil
}

// actionPins applies the pin rules to steps: no step writes a go-version, setup-go reads go.mod,
// and golangci-lint-action installs an exact version. It returns the pinned values by action, and a
// problem for every step that breaks a rule.
func actionPins(steps []actionStep) (values map[string][]pinValue, problems []string) {
	values = map[string][]pinValue{}

	for _, step := range steps {
		if step.with["go-version"] != "" {
			problems = append(problems, fmt.Sprintf(
				"%s: %s gets go-version %q; the Go version comes from go.mod",
				step.file, step.action, step.with["go-version"]))
		}

		if step.action == setupGo && step.with["go-version-file"] != "go.mod" {
			problems = append(problems, step.file+": setup-go must read go-version-file: go.mod")
		}

		if step.action == golangciLintA {
			version := step.with["version"]
			if !exactTag.MatchString(version) {
				problems = append(problems, fmt.Sprintf(
					"%s: golangci-lint-action must install an exact version, got %q",
					step.file, version))

				continue
			}

			values[golangciLintA] = append(values[golangciLintA], pinValue{version, step.file})
		}
	}

	return values, problems
}

// expectOneValue fails unless every source has the pin and all found values, the extra ones
// included, agree.
func expectOneValue(t *testing.T, what string, sources []pinSource, extra []pinValue) {
	t.Helper()

	root := toolgate.RepoRoot(t)
	found := map[string][]string{}

	for _, source := range sources {
		content, err := os.ReadFile(filepath.Join(root, filepath.FromSlash(source.file)))
		if err != nil {
			t.Errorf("%s: %v", what, err)

			continue
		}

		matches := source.pattern.FindAllStringSubmatch(string(content), -1)
		if len(matches) == 0 {
			t.Errorf("%s: %s does not write it (%s)", what, source.file, source.pattern)
		}

		for _, match := range matches {
			found[match[1]] = append(found[match[1]], source.file)
		}
	}

	if problem := expectOneValueIn(found, extra); problem != "" {
		t.Errorf("%s is not one value: %s", what, problem)
	}
}

// expectOneValueIn adds extra to found and describes the disagreement, or returns "" when every
// value is the same.
func expectOneValueIn(found map[string][]string, extra []pinValue) string {
	for _, value := range extra {
		found[value.value] = append(found[value.value], value.file)
	}

	if len(found) <= 1 {
		return ""
	}

	values := make([]string, 0, len(found))
	for value, files := range found {
		values = append(values, value+" in "+strings.Join(slices.Compact(slices.Sorted(
			slices.Values(files))), ", "))
	}

	slices.Sort(values)

	return strings.Join(values, "; ")
}
