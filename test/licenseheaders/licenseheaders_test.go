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

// Package licenseheaders_test drives scripts/check-license-headers.sh: on this repository, and on
// scratch repositories that hold each kind of file it must reject or let through.
package licenseheaders_test

import (
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"

	"github.com/leinardi/swarm-scheduler-exporter/test/internal/gitenv"
	"github.com/leinardi/swarm-scheduler-exporter/test/internal/toolgate"
)

const scriptPath = "scripts/check-license-headers.sh"

// goHeader carries the lines the check looks for; its last one is the eighth line.
const goHeader = `/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * The above copyright notice and this permission notice shall be included in all
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 */

package x
`

// apacheHeader is another licence's notice with the right copyright line.
const apacheHeader = `/*
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 */

package x
`

const shellHeader = `#!/usr/bin/env bash
#
# MIT License
#
# Copyright (c) 2025-2026 Roberto Leinardi
#
# Permission is hereby granted, free of charge, to any person obtaining a copy
# The above copyright notice and this permission notice shall be included in all
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
echo ok
`

const makefile = "MK_COMMON_FILES       := docker.mk help.mk\n"

// makefileWithHeader is the scratch repositories' Makefile, which the check governs too.
var makefileWithHeader = strings.TrimPrefix(shellHeader, "#!/usr/bin/env bash\n") + makefile

// untrackedPrefix marks scratch files that are written but never added: the check reads tracked
// files only.
const untrackedPrefix = "untracked/"

// TestRepositoryHeaders runs the check on this repository.
func TestRepositoryHeaders(t *testing.T) {
	t.Parallel()

	root := toolgate.RepoRoot(t)
	readScript(t, root)
	readTrackedFiles(t, root)

	// The repository's own git environment is the right one here: under the commit hook it names
	// the index being committed.
	cmd := exec.CommandContext(t.Context(), scriptPath)
	cmd.Dir = root

	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Errorf("the repository fails its own license header check: %v\n%s", err, out)
	}
}

// TestHeaderCases runs the check on scratch repositories, one file kind at a time.
func TestHeaderCases(t *testing.T) {
	t.Parallel()

	script := readScript(t, toolgate.RepoRoot(t))

	for name, test := range map[string]struct {
		files map[string]string
		pass  bool
	}{
		"go file with the header": {map[string]string{"internal/x/x.go": goHeader}, true},
		"go file after a build constraint": {
			map[string]string{"internal/x/x.go": "//go:build integration\n\n" + goHeader},
			true,
		},
		"go file without a header": {map[string]string{"internal/x/x.go": "package x\n"}, false},
		"another licence's header": {map[string]string{"internal/x/x.go": apacheHeader}, false},
		"another copyright holder": {
			map[string]string{
				"internal/x/x.go": strings.ReplaceAll(goHeader, "Roberto Leinardi", "Someone Else"),
			},
			false,
		},
		// Rust carries the same /* */ block as Go.
		"rust file with the header": {
			map[string]string{"src/x.rs": strings.Replace(goHeader, "package x\n", "fn x() {}\n", 1)},
			true,
		},
		"rust file without a header": {map[string]string{"src/x.rs": "fn x() {}\n"}, false},
		"rust file with another copyright holder": {
			map[string]string{
				"meta/src/x.rs": strings.ReplaceAll(goHeader, "Roberto Leinardi", "Someone Else"),
			},
			false,
		},
		"rust testdata fixture": {
			map[string]string{"tests/testdata/fixture.rs": "fn x() {}\n"},
			true,
		},
		"shell script with the header": {map[string]string{"scripts/a.sh": shellHeader}, true},
		"shell script without a header": {
			map[string]string{"scripts/a.sh": "#!/usr/bin/env bash\necho\n"},
			false,
		},
		"dockerfile with the header after the syntax line": {
			map[string]string{
				"deployments/docker/Dockerfile": "# syntax=docker/dockerfile:1.7\n" +
					strings.TrimPrefix(shellHeader, "#!/usr/bin/env bash\n"),
			},
			true,
		},
		"dockerfile without a header": {
			map[string]string{"deployments/docker/Dockerfile": "FROM scratch\n"},
			false,
		},
		"local make fragment, no header": {map[string]string{".mk/verify.mk": "x:\n"}, false},
		"rust make fragment, no header":  {map[string]string{".mk/rust.mk": "x:\n"}, false},
		"makefile without a header":      {map[string]string{"Makefile": makefile}, false},
		// goHeader's last required line is its eighth: 22 lines before it put it on line 30, the
		// last line of the window, and one more pushes it out.
		"header ending on the last line of the window": {
			map[string]string{"internal/x/x.go": strings.Repeat("// filler\n", 22) + goHeader},
			true,
		},
		"header ending one line past the window": {
			map[string]string{"internal/x/x.go": strings.Repeat("// filler\n", 23) + goHeader},
			false,
		},
		"untracked file without a header": {
			map[string]string{untrackedPrefix + "scratch.go": "package x\n"},
			true,
		},
		"upstream make fragment": {
			map[string]string{".mk/docker.mk": "x:\n", ".mk/help.mk": "y:\n"},
			true,
		},
		"bootstrap script": {
			map[string]string{"scripts/bootstrap-mk-common.sh": "#!/bin/sh\n"},
			true,
		},
		"testdata fixture": {
			map[string]string{"internal/x/testdata/fixture.go": "package fixture\n"},
			true,
		},
		"markdown, yaml and json": {
			map[string]string{"README.md": "# x\n", "a.yaml": "a: 1\n", "b.json": "{}\n"},
			true,
		},
	} {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			dir := scratchRepo(t, script, test.files)

			out, err := runIn(t, dir)
			if test.pass && err != nil {
				t.Errorf("rejected, want accepted: %v\n%s", err, out)
			}

			if !test.pass && err == nil {
				t.Errorf("accepted, want rejected:\n%s", out)
			}
		})
	}
}

// readTrackedFiles reads the index and every tracked file, so a change to any of them is part of
// the test cache key and a cached pass is not replayed after a headerless file is added.
func readTrackedFiles(t *testing.T, root string) {
	t.Helper()

	for _, args := range [][]string{{"rev-parse", "--git-path", "index"}, {"ls-files"}} {
		cmd := exec.CommandContext(t.Context(), "git", args...)
		cmd.Dir = root

		out, err := cmd.Output()
		if err != nil {
			t.Fatalf("git %v: %v", args, err)
		}

		for name := range strings.SplitSeq(strings.TrimSpace(string(out)), "\n") {
			if !filepath.IsAbs(name) {
				name = filepath.Join(root, name)
			}

			// A deleted tracked file is the script's concern, not this one's.
			_, _ = os.ReadFile(name)
		}
	}
}

// readScript reads the checker, which also makes it part of the test cache key: a program run only
// through exec is not, and an edited script would otherwise replay a cached pass.
func readScript(t *testing.T, root string) []byte {
	t.Helper()

	data, err := os.ReadFile(filepath.Join(root, scriptPath))
	if err != nil {
		t.Fatalf("read the checker: %v", err)
	}

	return data
}

// scratchRepo creates a git repository holding the checker, a Makefile and files.
func scratchRepo(t *testing.T, script []byte, files map[string]string) string {
	t.Helper()

	dir := t.TempDir()

	root, err := os.OpenRoot(dir)
	if err != nil {
		t.Fatalf("open %s: %v", dir, err)
	}

	defer func() { _ = root.Close() }()

	files[scriptPath] = string(script)
	if _, ok := files["Makefile"]; !ok {
		files["Makefile"] = makefileWithHeader
	}

	for name, content := range files {
		err = root.MkdirAll(filepath.Dir(name), 0o750)
		if err != nil {
			t.Fatalf("create the directory of %s: %v", name, err)
		}

		err = root.WriteFile(name, []byte(content), 0o600)
		if err != nil {
			t.Fatalf("write %s: %v", name, err)
		}
	}

	err = root.Chmod(scriptPath, 0o700)
	if err != nil {
		t.Fatalf("make the checker executable: %v", err)
	}

	git(t, dir, "init", "--quiet")

	for name := range files {
		if !strings.HasPrefix(name, untrackedPrefix) {
			git(t, dir, "add", "--", name)
		}
	}

	return dir
}

// runIn runs the checker kept at scriptPath inside the repository at dir.
func runIn(t *testing.T, dir string) (string, error) {
	t.Helper()

	// A relative command path is resolved against Dir.
	cmd := exec.CommandContext(t.Context(), scriptPath)
	cmd.Dir = dir
	cmd.Env = gitenv.Isolated()

	out, err := cmd.CombinedOutput()

	return string(out), err
}

func git(t *testing.T, dir string, args ...string) {
	t.Helper()

	cmd := exec.CommandContext(t.Context(), "git", args...)
	cmd.Dir = dir
	cmd.Env = gitenv.Isolated()

	out, err := cmd.CombinedOutput()
	if err != nil {
		t.Fatalf("git %v: %v\n%s", args, err, out)
	}
}
