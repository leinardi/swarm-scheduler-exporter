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

// Package dockerfiles reads Dockerfiles closely enough to assert the image invariants that
// docs/release.md and the Dockerfile's own comments state: stages, their bases and the
// instructions inside them.
package dockerfiles

import (
	"errors"
	"fmt"
	"regexp"
	"strings"
)

// errMalformed reports a Dockerfile this reader cannot take apart; the assertions refuse it rather
// than guess.
var errMalformed = errors.New("malformed Dockerfile")

// Instruction is one Dockerfile instruction with its continuation lines joined.
type Instruction struct {
	// Keyword is the instruction, upper-cased: FROM, RUN, COPY, ...
	Keyword string
	// Args is everything after the keyword, with continuations joined by a space.
	Args string
}

// Stage is one FROM and the instructions up to the next.
type Stage struct {
	// Name is the AS alias, empty for an unnamed stage.
	Name string
	// Platform is the --platform value, empty when the stage has none.
	Platform string
	// Image is the base reference exactly as written.
	Image string
	// Instructions excludes the FROM itself.
	Instructions []Instruction
}

// digestPinned is a literal registry reference with a tag and a sha256 digest. A variable anywhere
// in the reference makes it unpinnable by a reader: the value lives in a build argument.
var digestPinned = regexp.MustCompile(`^[a-z0-9.\-/:]+:[A-Za-z0-9._\-]+@sha256:[0-9a-f]{64}$`)

// tagPinned is a literal registry reference with a tag and no digest: the policy for the dhi.io
// bases, which dhi.io republishes with security fixes (docs/release.md).
var tagPinned = regexp.MustCompile(`^[a-z0-9.\-/:]+:[A-Za-z0-9._\-]+$`)

// parserDirective is one "# key=value" line of the block BuildKit reads before anything else.
var parserDirective = regexp.MustCompile(`^#\s*([A-Za-z]+)\s*=\s*(\S+)\s*$`)

// Syntax returns the value of the file's syntax parser directive. BuildKit reads directives only
// from the file's leading lines, and stops at the first line that is not one (a comment, a blank
// line or an instruction), so a "# syntax=" line anywhere later is an ordinary comment: this
// reader stops at the same place, and reports the directive missing rather than find one BuildKit
// ignores.
func Syntax(content string) (string, bool) {
	for line := range strings.SplitSeq(content, "\n") {
		match := parserDirective.FindStringSubmatch(strings.TrimSpace(line))
		if match == nil {
			return "", false
		}

		if strings.EqualFold(match[1], "syntax") {
			return match[2], true
		}
	}

	return "", false
}

// Parse splits a Dockerfile into instructions, dropping comments and blank lines and joining
// backslash continuations, and groups them into stages.
func Parse(content string) ([]Stage, error) {
	var (
		stages  []Stage
		pending string
	)

	for line := range strings.SplitSeq(content, "\n") {
		trimmed := strings.TrimSpace(line)

		// A comment line is dropped even inside a continuation, as BuildKit does.
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}

		if body, continued := strings.CutSuffix(trimmed, "\\"); continued {
			pending += body + " "

			continue
		}

		instruction := strings.Join(strings.Fields(pending+trimmed), " ")
		pending = ""

		keyword, args, _ := strings.Cut(instruction, " ")
		keyword = strings.ToUpper(keyword)

		if keyword == "FROM" {
			stage, err := parseFrom(args)
			if err != nil {
				return nil, err
			}

			stages = append(stages, stage)

			continue
		}

		if len(stages) == 0 {
			if keyword == "ARG" {
				continue
			}

			return nil, fmt.Errorf("%w: %s before the first FROM", errMalformed, keyword)
		}

		last := &stages[len(stages)-1]
		last.Instructions = append(last.Instructions, Instruction{Keyword: keyword, Args: args})
	}

	if strings.TrimSpace(pending) != "" {
		return nil, fmt.Errorf("%w: the file ends inside a continuation", errMalformed)
	}

	return stages, nil
}

func parseFrom(args string) (Stage, error) {
	var stage Stage

	fields := strings.Fields(args)

	for len(fields) > 0 && strings.HasPrefix(fields[0], "--") {
		if platform, ok := strings.CutPrefix(fields[0], "--platform="); ok {
			stage.Platform = platform
		}

		fields = fields[1:]
	}

	switch {
	case len(fields) == 1:
		stage.Image = fields[0]
	case len(fields) == 3 && strings.EqualFold(fields[1], "AS"):
		stage.Image = fields[0]
		stage.Name = fields[2]
	default:
		return Stage{}, fmt.Errorf("%w: FROM %s", errMalformed, args)
	}

	return stage, nil
}

// DigestPinned reports whether a base reference is literal and pinned by digest.
func DigestPinned(image string) bool {
	return digestPinned.MatchString(image)
}

// TagPinned reports whether a base reference is literal and pinned by a tag other than latest,
// without a digest.
func TagPinned(image string) bool {
	return tagPinned.MatchString(image) && Repository(image) != image && Tag(image) != "latest"
}

// Repository returns the reference without its tag and digest: dhi.io/golang for
// dhi.io/golang:1.26.9-alpine3.23-dev@sha256:...
func Repository(image string) string {
	name, _, _ := strings.Cut(image, "@")

	slash := strings.LastIndex(name, "/")
	if colon := strings.LastIndex(name, ":"); colon > slash {
		name = name[:colon]
	}

	return name
}

// Tag returns the tag of a reference, empty when it has none.
func Tag(image string) string {
	name, _, _ := strings.Cut(image, "@")

	slash := strings.LastIndex(name, "/")
	if colon := strings.LastIndex(name, ":"); colon > slash {
		return name[colon+1:]
	}

	return ""
}

// Labels returns the LABEL key/value pairs of a stage, with the quotes around values removed.
func Labels(stage Stage) map[string]string {
	labels := map[string]string{}
	pair := regexp.MustCompile(`([A-Za-z0-9._\-]+)=("(?:[^"\\]|\\.)*"|\S+)`)

	for _, instruction := range stage.Instructions {
		if instruction.Keyword != "LABEL" {
			continue
		}

		for _, match := range pair.FindAllStringSubmatch(instruction.Args, -1) {
			labels[match[1]] = strings.Trim(match[2], `"`)
		}
	}

	return labels
}
