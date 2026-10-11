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

// Package docs checks the Markdown documentation offline: every relative link and fragment
// resolves. It needs no network and no tool beyond the Go toolchain, so it runs as a pre-commit
// hook.
package docs

import (
	"fmt"
	"os"
	"path"
	"path/filepath"
	"regexp"
	"strings"
	"unicode"
)

var (
	inlineLink    = regexp.MustCompile(`!?\[[^\]]*\]\(\s*(<[^>]*>|[^)\s]+)(?:\s+"[^"]*")?\s*\)`)
	referenceDef  = regexp.MustCompile(`^\s{0,3}\[[^\]]+\]:\s*(<[^>]*>|\S+)`)
	atxHeading    = regexp.MustCompile(`^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$`)
	htmlAnchor    = regexp.MustCompile(`<a\s+(?:id|name)="([^"]+)"`)
	codeSpan      = regexp.MustCompile("`+[^`]*`+")
	linkInHeading = regexp.MustCompile(`!?\[([^\]]*)\]\([^)]*\)`)
	htmlTag       = regexp.MustCompile(`<[^>]+>`)
	edgeUnderline = regexp.MustCompile(`(^|[^\p{L}\p{N}])_+|_+([^\p{L}\p{N}]|$)`)
	uriScheme     = regexp.MustCompile(`^[a-zA-Z][a-zA-Z0-9+.-]*:`)
)

// Problem is one broken link, reported against the file and line that contains it.
type Problem struct {
	File string
	Line int
	Msg  string
}

func (p Problem) String() string {
	return fmt.Sprintf("%s:%d: %s", p.File, p.Line, p.Msg)
}

// Lines returns the lines of a Markdown file that are prose, with fenced code blocks blanked out so
// line numbers still match the file.
func Lines(content string) []string {
	lines := strings.Split(content, "\n")
	fence := ""

	for index, line := range lines {
		trimmed := strings.TrimSpace(line)

		if fence != "" {
			if strings.HasPrefix(trimmed, fence) {
				fence = ""
			}

			lines[index] = ""

			continue
		}

		for _, marker := range []string{"```", "~~~"} {
			if strings.HasPrefix(trimmed, marker) {
				fence = marker
				lines[index] = ""
			}
		}
	}

	return lines
}

// Anchors returns the fragments a Markdown file defines: its headings under GitHub's slug rules,
// numbered on repetition, and any explicit HTML anchors.
func Anchors(content string) map[string]bool {
	anchors := map[string]bool{}
	seen := map[string]int{}

	for _, line := range Lines(content) {
		for _, match := range htmlAnchor.FindAllStringSubmatch(line, -1) {
			anchors[match[1]] = true
		}

		heading := atxHeading.FindStringSubmatch(line)
		if heading == nil {
			continue
		}

		slug := Slug(heading[2])
		if count := seen[slug]; count > 0 {
			anchors[fmt.Sprintf("%s-%d", slug, count)] = true
		} else {
			anchors[slug] = true
		}

		seen[slug]++
	}

	return anchors
}

// Slug turns heading text into the anchor GitHub generates for it: the rendered text, lower-cased,
// with everything but letters, marks, numbers, connector punctuation, spaces and hyphens removed,
// and each space turned into a hyphen.
func Slug(heading string) string {
	text := renderText(heading)

	var builder strings.Builder

	for _, char := range strings.ToLower(text) {
		switch {
		case char == ' ':
			builder.WriteRune('-')
		case char == '-',
			unicode.IsLetter(char),
			unicode.IsMark(char),
			unicode.IsNumber(char),
			unicode.Is(unicode.Pc, char):
			builder.WriteRune(char)
		}
	}

	return builder.String()
}

// renderText strips the Markdown syntax that does not reach the rendered heading text: link
// targets, HTML tags, emphasis markers and code-span backticks (keeping the code itself).
func renderText(heading string) string {
	var builder strings.Builder

	last := 0

	for _, span := range codeSpan.FindAllStringIndex(heading, -1) {
		builder.WriteString(stripInline(heading[last:span[0]]))
		builder.WriteString(strings.Trim(heading[span[0]:span[1]], "`"))

		last = span[1]
	}

	builder.WriteString(stripInline(heading[last:]))

	return builder.String()
}

func stripInline(text string) string {
	text = linkInHeading.ReplaceAllString(text, "$1")
	text = htmlTag.ReplaceAllString(text, "")
	text = strings.ReplaceAll(text, "*", "")

	return edgeUnderline.ReplaceAllString(text, "$1$2")
}

// Targets returns the link targets on one prose line, inline code spans excluded.
func Targets(line string) []string {
	line = codeSpan.ReplaceAllString(line, "")

	var targets []string

	for _, match := range inlineLink.FindAllStringSubmatch(line, -1) {
		targets = append(targets, strings.Trim(match[1], "<>"))
	}

	if match := referenceDef.FindStringSubmatch(line); match != nil {
		targets = append(targets, strings.Trim(match[1], "<>"))
	}

	return targets
}

// Checker resolves links against a repository root.
type Checker struct {
	Root    string
	anchors map[string]map[string]bool
}

// NewChecker returns a checker for the repository at root.
func NewChecker(root string) *Checker {
	return &Checker{Root: root, anchors: map[string]map[string]bool{}}
}

// CheckFile checks every relative link in the Markdown file at rel, a slash-separated path relative
// to the root. Paths are resolved logically, against rel's own directory, and then followed through
// any symlink on disk.
func (c *Checker) CheckFile(rel string) ([]Problem, error) {
	data, err := os.ReadFile(filepath.Join(c.Root, filepath.FromSlash(rel)))
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", rel, err)
	}

	var problems []Problem

	for i, line := range Lines(string(data)) {
		for _, target := range Targets(line) {
			msg := c.checkTarget(rel, target)
			if msg != "" {
				problems = append(problems, Problem{File: rel, Line: i + 1, Msg: msg})
			}
		}
	}

	return problems, nil
}

func (c *Checker) checkTarget(rel, target string) string {
	if uriScheme.MatchString(target) || strings.HasPrefix(target, "//") {
		return ""
	}

	file, fragment, _ := strings.Cut(target, "#")
	resolved := rel

	if file != "" {
		if absolute, ok := strings.CutPrefix(file, "/"); ok {
			resolved = path.Clean(absolute)
		} else {
			resolved = path.Join(path.Dir(rel), file)
		}

		if strings.HasPrefix(resolved, "../") || resolved == ".." {
			return fmt.Sprintf("link %q leaves the repository", target)
		}

		_, err := os.Stat(filepath.Join(c.Root, filepath.FromSlash(resolved)))
		if err != nil {
			return fmt.Sprintf("link %q: %s does not exist", target, resolved)
		}
	}

	if fragment == "" || !strings.HasSuffix(resolved, ".md") {
		return ""
	}

	anchors, err := c.anchorsOf(resolved)
	if err != nil {
		return fmt.Sprintf("link %q: %v", target, err)
	}

	if !anchors[fragment] {
		return fmt.Sprintf("link %q: %s has no heading or anchor #%s", target, resolved, fragment)
	}

	return ""
}

func (c *Checker) anchorsOf(rel string) (map[string]bool, error) {
	if anchors, ok := c.anchors[rel]; ok {
		return anchors, nil
	}

	data, err := os.ReadFile(filepath.Join(c.Root, filepath.FromSlash(rel)))
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", rel, err)
	}

	anchors := Anchors(string(data))
	c.anchors[rel] = anchors

	return anchors, nil
}
