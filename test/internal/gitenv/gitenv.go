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

// Package gitenv gives meta-tests an environment for git commands that cannot reach the
// repository being committed.
//
// The Go tests also run inside a pre-commit hook, and git exports its own location to hooks:
// GIT_INDEX_FILE always, GIT_DIR and others in some setups, absolute in a linked worktree. A git
// command run in a scratch repository with that environment acts on the real one instead -- a git
// add there writes into the index of the commit being made. Isolated strips those variables and
// the user's own configuration, so a scratch repository is only ever itself.
package gitenv

import (
	"os"
	"slices"
	"strings"
)

// repositoryVars point git at a repository other than the working directory's.
var repositoryVars = []string{
	"GIT_DIR",
	"GIT_INDEX_FILE",
	"GIT_WORK_TREE",
	"GIT_OBJECT_DIRECTORY",
	"GIT_ALTERNATE_OBJECT_DIRECTORIES",
	"GIT_COMMON_DIR",
	"GIT_PREFIX",
	"GIT_NAMESPACE",
	"GIT_CEILING_DIRECTORIES",
}

// Isolated returns the process environment without the variables that redirect git, without the
// user's global and system configuration, and with extra appended.
func Isolated(extra ...string) []string {
	env := make([]string, 0, len(os.Environ())+len(extra)+2)

	for _, entry := range os.Environ() {
		name, _, _ := strings.Cut(entry, "=")
		if slices.Contains(repositoryVars, name) || strings.HasPrefix(name, "GIT_CONFIG") {
			continue
		}

		env = append(env, entry)
	}

	env = append(env, "GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_NOSYSTEM=1")

	return append(env, extra...)
}
