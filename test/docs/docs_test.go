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

package docs_test

import (
	"os"
	"os/exec"
	"slices"
	"strings"
	"testing"

	"github.com/leinardi/swarm-scheduler-exporter/test/docs"
	"github.com/leinardi/swarm-scheduler-exporter/test/internal/toolgate"
)

// TestRelativeLinksResolve checks every relative link and fragment in the project's Markdown.
func TestRelativeLinksResolve(t *testing.T) {
	t.Parallel()

	root := toolgate.RepoRoot(t)
	checker := docs.NewChecker(root)

	for _, rel := range markdownFiles(t, root) {
		problems, err := checker.CheckFile(rel)
		if err != nil {
			t.Fatal(err)
		}

		for _, problem := range problems {
			t.Error(problem)
		}
	}
}

// TestCheckerFindsBrokenLinks proves the link check is not blind, on a scratch tree.
func TestCheckerFindsBrokenLinks(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	writeFixture(t, dir, "a.md", strings.Join([]string{
		"# Title",
		"## 1. Section — `GET /x`",
		"[ok](b.md#heading-two) [ok](#1-section--get-x) [ok](https://example.com/missing)",
		"[broken file](missing.md) [broken anchor](b.md#nope) `[in code](missing-too.md)`",
		"```text",
		"[in a fence](missing-three.md)",
		"```",
	}, "\n"))
	writeFixture(t, dir, "b.md", "# Heading Two\n")

	problems, err := docs.NewChecker(dir).CheckFile("a.md")
	if err != nil {
		t.Fatal(err)
	}

	got := make([]string, 0, len(problems))
	for _, problem := range problems {
		got = append(got, problem.String())
	}

	want := []string{
		`a.md:4: link "missing.md": missing.md does not exist`,
		`a.md:4: link "b.md#nope": b.md has no heading or anchor #nope`,
	}
	if !slices.Equal(got, want) {
		t.Errorf("problems = %q, want %q", got, want)
	}
}

// TestSlug pins the GitHub slug rules the anchors follow.
func TestSlug(t *testing.T) {
	t.Parallel()

	for heading, want := range map[string]string{
		"1. Test (Home) — `/`":                        "1-test-home--",
		"5. `GET /api/v1/info`":                       "5-get-apiv1info",
		"5.1 Implementation":                          "51-implementation",
		"Why the Build Happens Before the Tag":        "why-the-build-happens-before-the-tag",
		"11.2 Path A fallback — HTTP probe":           "112-path-a-fallback--http-probe",
		"5. History Module (`idb`)":                   "5-history-module-idb",
		"**Bold** and _emphasis_ with snake_case":     "bold-and-emphasis-with-snake_case",
		"[Linked](other.md) heading":                  "linked-heading",
		"2.2 Stateless Server":                        "22-stateless-server",
		"Tags, Moves and `X.Y.Z`":                     "tags-moves-and-xyz",
		"Ünïcode Lëtters Stay":                        "ünïcode-lëtters-stay",
		"Required Checks and Fork Pull Requests":      "required-checks-and-fork-pull-requests",
		"6. `compat.networkquality.max_upload_bytes`": "6-compatnetworkqualitymax_upload_bytes",
		"💡 What is “Swarm Scheduler Exporter”?":       "-what-is-swarm-scheduler-exporter",
	} {
		if got := docs.Slug(heading); got != want {
			t.Errorf("Slug(%q) = %q, want %q", heading, got, want)
		}
	}
}

// markdownFiles returns the tracked Markdown as slash-separated paths relative to root. Reading the
// list from git keeps an untracked scratch file out; -count=1 on the hook keeps the list fresh.
func markdownFiles(t *testing.T, root string) []string {
	t.Helper()

	cmd := exec.CommandContext(t.Context(), "git", "ls-files", "-z", "--", "*.md")
	cmd.Dir = root

	out, err := cmd.Output()
	if err != nil {
		t.Fatalf("git ls-files: %v", err)
	}

	var files []string

	for rel := range strings.SplitSeq(strings.TrimRight(string(out), "\x00"), "\x00") {
		if rel != "" {
			files = append(files, rel)
		}
	}

	if len(files) == 0 {
		t.Fatal("git ls-files lists no Markdown; the check would pass on nothing")
	}

	return files
}

func writeFixture(t *testing.T, dir, name, content string) {
	t.Helper()

	root, err := os.OpenRoot(dir)
	if err != nil {
		t.Fatalf("open %s: %v", dir, err)
	}

	defer func() { _ = root.Close() }()

	err = root.WriteFile(name, []byte(content), 0o600)
	if err != nil {
		t.Fatalf("write %s: %v", name, err)
	}
}
