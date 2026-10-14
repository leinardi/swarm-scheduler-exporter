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
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"
	"testing"

	"github.com/leinardi/swarm-scheduler-exporter/test/internal/toolgate"
)

// vacuousFlags turn a gate with nothing to check, or a failing one, into a pass. None of these may
// appear where gates are defined, outside the allowlist below.
var vacuousFlags = []struct {
	name    string
	pattern *regexp.Regexp
}{
	{"|| true", regexp.MustCompile(`\|\|\s*(true\b|:(\s|;|$)|exit\s+0\b)`)},
	{"make's - recipe prefix", regexp.MustCompile(`^\t[@+]*-`)},
	{".IGNORE", regexp.MustCompile(`^\.IGNORE\b`)},
	{
		"make --ignore-errors",
		regexp.MustCompile(`\$\(MAKE\).*\s(-[a-zA-Z]*i[a-zA-Z]*|--ignore-errors)\b`),
	},
	{"continue-on-error", regexp.MustCompile(`continue-on-error`)},
	{
		"if: false",
		regexp.MustCompile(`\bif:\s*(["']?false["']?\s*$|["']?\$\{\{\s*false\s*\}\}["']?)`),
	},
}

// allowedFragment is one exact piece of shell, in one file, whose || true is not a gate passing on
// nothing; the reason says why. A line is allowed only when the flag is gone once the fragment is
// removed, so a second || true on the same line is still caught.
type allowedFragment struct {
	file     string
	fragment string
	reason   string
}

var allowedFragments = []allowedFragment{
	{
		file:     ".github/workflows/release.yaml",
		fragment: `{ grep -E "$RELEASE_TAG_REGEX" || true; }`,
		reason: "a tag list with no release tag in it is an empty answer, not an error; grep " +
			"exits 1 on no match and pipefail would turn that into a failed lookup",
	},
	{
		file:     ".github/workflows/release.yaml",
		fragment: `{ git tag --merged "HEAD^" 2>/dev/null || true; }`,
		reason:   "HEAD^ does not exist on a root commit, which has no previous release",
	},
}

// TestNoVacuousFlags scans the files that define gates: the Makefile and the make fragments, the
// pre-commit configuration and every YAML file under .github, the workflows included.
func TestNoVacuousFlags(t *testing.T) {
	t.Parallel()

	root := toolgate.RepoRoot(t)

	fragments := glob(t, root, ".mk", ".mk")
	if len(fragments) == 0 {
		t.Fatal("no make fragments found under .mk/; the scan would check nothing")
	}

	workflows := githubYAML(t)

	files := make([]string, 0, 2+len(fragments)+len(workflows))
	files = append(files, "Makefile", ".pre-commit-config.yaml")
	files = append(files, fragments...)
	files = append(files, workflows...)

	used := map[int]bool{}

	for _, rel := range files {
		for _, hit := range vacuousHits(rel, readRepoFile(t, rel), used) {
			t.Errorf("%s:%s", rel, hit)
		}
	}

	for index, allowed := range allowedFragments {
		if !used[index] {
			t.Errorf("the allowlist entry %q in %s (%s) matches nothing; remove it",
				allowed.fragment, allowed.file, allowed.reason)
		}
	}
}

// TestVacuousFlagsAreRecognized is the negative control: each flag, written as a gate would carry
// it, is found, and the allowlist covers only its own fragment in its own file.
func TestVacuousFlagsAreRecognized(t *testing.T) {
	t.Parallel()

	for _, sample := range []string{
		"\tgo vet ./... || true",
		"\tgo vet ./... ||:",
		"\tgo vet ./... || exit 0",
		"\t-go vet ./...",
		"\t@-go vet ./...",
		".IGNORE: go-vet",
		"\t$(MAKE) -i go-test",
		"\t$(MAKE) --ignore-errors go-test",
		"        continue-on-error: true",
		"    if: false",
		"    if: ${{ false }}",
		"    if: 'false'",
		"    if: \"${{ false }}\"",
	} {
		if len(vacuousHits("Makefile", sample, map[int]bool{})) == 0 {
			t.Errorf("no vacuous flag found in %q", sample)
		}
	}

	release := ".github/workflows/release.yaml"
	allowed := `  { grep -E "$RELEASE_TAG_REGEX" || true; } | sort -V`

	if hits := vacuousHits(release, allowed, map[int]bool{}); len(hits) != 0 {
		t.Errorf("the allowed fragment was flagged: %v", hits)
	}

	for file, sample := range map[string]string{
		".github/workflows/ci.yaml": allowed,
		release:                     allowed + " || true",
		".github/workflows/x.yaml":  `  { git tag --merged "HEAD^" 2>/dev/null || true; }`,
	} {
		if len(vacuousHits(file, sample, map[int]bool{})) == 0 {
			t.Errorf("%s: %q passed through the allowlist", file, sample)
		}
	}
}

// vacuousHits returns the flags on content's lines, after removing the fragments allowed in file;
// used records which allowlist entries matched.
func vacuousHits(file, content string, used map[int]bool) []string {
	var hits []string

	for index, line := range strings.Split(content, "\n") {
		for allowedIndex, allowed := range allowedFragments {
			if allowed.file == file && strings.Contains(line, allowed.fragment) {
				line = strings.ReplaceAll(line, allowed.fragment, "")
				used[allowedIndex] = true
			}
		}

		for _, flag := range vacuousFlags {
			if flag.pattern.MatchString(line) {
				hits = append(hits, strconv.Itoa(index+1)+": "+flag.name+": "+
					strings.TrimSpace(line))
			}
		}
	}

	return hits
}

// glob lists the files directly in dir (relative to root) with one of the suffixes, as paths
// relative to root; a missing directory lists nothing.
func glob(t *testing.T, root, dir string, suffixes ...string) []string {
	t.Helper()

	entries, err := os.ReadDir(filepath.Join(root, dir))
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}

		t.Fatalf("list %s: %v", dir, err)
	}

	var files []string

	for _, entry := range entries {
		if entry.Type()&fs.ModeType != 0 {
			continue
		}

		for _, suffix := range suffixes {
			if strings.HasSuffix(entry.Name(), suffix) {
				files = append(files, dir+"/"+entry.Name())
			}
		}
	}

	return files
}
