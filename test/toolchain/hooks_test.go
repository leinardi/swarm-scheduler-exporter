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
	"slices"
	"testing"
)

// alwaysRunHooks read the whole tree rather than the files pre-commit passes, so each one runs on
// every commit and must not run twice per commit.
var alwaysRunHooks = []string{"check-license-headers", "check-suppressions", "docs-links"}

// TestAlwaysRunHooksRunOnce: a whole-tree hook takes no file list and always runs, because a commit
// that only deletes a file passes no file to it; and it is pinned to the pre-commit stage, because
// always_run makes a hook eligible at every installed stage, commit-msg included.
func TestAlwaysRunHooksRunOnce(t *testing.T) {
	t.Parallel()

	hooks := preCommitHooks(t)

	for _, id := range alwaysRunHooks {
		found := hooks[id]
		if len(found) != 1 {
			t.Errorf("hook %s appears %d times, want once", id, len(found))

			continue
		}

		hook := found[0]

		if hook["always_run"] != true {
			t.Errorf("hook %s does not set always_run: true", id)
		}

		if hook["pass_filenames"] != false {
			t.Errorf("hook %s does not set pass_filenames: false", id)
		}

		stages, _ := hook["stages"].([]any)
		if !slices.Equal(stages, []any{"pre-commit"}) {
			t.Errorf("hook %s runs at stages %v, want exactly [pre-commit]", id, hook["stages"])
		}
	}
}
