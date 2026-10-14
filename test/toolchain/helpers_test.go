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

// Package toolchain_test pins the repository's tooling: the shared version pins, Docker staying out
// of untagged tests, the always-run hooks running once, and the absence of flags that make a gate
// pass on nothing.
package toolchain_test

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"go.yaml.in/yaml/v3"

	"github.com/leinardi/swarm-scheduler-exporter/test/internal/toolgate"
)

// readRepoFile reads a file by its slash-separated path relative to the repository root.
func readRepoFile(t *testing.T, rel string) string {
	t.Helper()

	data, err := os.ReadFile(filepath.Join(toolgate.RepoRoot(t), filepath.FromSlash(rel)))
	if err != nil {
		t.Fatalf("read %s: %v", rel, err)
	}

	return string(data)
}

// preCommitHook is one hook's mapping as written; its keys are pre-commit's own snake_case names.
type preCommitHook map[string]any

// preCommitHooks parses .pre-commit-config.yaml and returns its hooks by id; an id with aliases
// (the actionlint, shellcheck and yamllint variants) appears more than once.
func preCommitHooks(t *testing.T) map[string][]preCommitHook {
	t.Helper()

	var config struct {
		Repos []struct {
			Hooks []preCommitHook `yaml:"hooks"`
		} `yaml:"repos"`
	}

	err := yaml.Unmarshal([]byte(readRepoFile(t, ".pre-commit-config.yaml")), &config)
	if err != nil {
		t.Fatalf("parse .pre-commit-config.yaml: %v", err)
	}

	hooks := map[string][]preCommitHook{}

	for _, repo := range config.Repos {
		for _, hook := range repo.Hooks {
			id, _ := hook["id"].(string)
			hooks[id] = append(hooks[id], hook)
		}
	}

	return hooks
}

// githubYAML lists every YAML file under .github, relative to the repository root.
func githubYAML(t *testing.T) []string {
	t.Helper()

	root := toolgate.RepoRoot(t)

	var files []string

	err := filepath.WalkDir(
		filepath.Join(root, ".github"),
		func(path string, entry fs.DirEntry, err error) error {
			if err != nil {
				return err
			}

			if !entry.IsDir() &&
				(strings.HasSuffix(path, ".yaml") || strings.HasSuffix(path, ".yml")) {
				rel, relErr := filepath.Rel(root, path)
				if relErr != nil {
					return fmt.Errorf("relative path of %s: %w", path, relErr)
				}

				files = append(files, filepath.ToSlash(rel))
			}

			return nil
		},
	)
	if err != nil {
		t.Fatal(err)
	}

	if len(files) == 0 {
		t.Fatal("no YAML found under .github; the checks would see nothing")
	}

	return files
}
