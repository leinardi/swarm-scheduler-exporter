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
	"go/ast"
	"go/build/constraint"
	"go/parser"
	"go/token"
	"io/fs"
	"path/filepath"
	"slices"
	"strconv"
	"strings"
	"testing"

	"github.com/leinardi/swarm-scheduler-exporter/test/internal/toolgate"
)

// integrationTag is the build tag every Docker-dependent file sits behind.
const integrationTag = "integration"

// integrationTrees hold the DinD harness and the suite: a Go file in them builds untagged only if
// it imports nothing but the standard library (the harness's sweep rules and redaction, tested
// without a daemon), or untagged go test would compile code that drives a real Docker daemon.
var integrationTrees = []string{"internal/testenv/", "test/integration/"}

// harnessPackage drives Docker; an untagged test that imports it would need a daemon.
const harnessPackage = "github.com/leinardi/swarm-scheduler-exporter/internal/testenv"

// goFile is one parsed Go file of the repository.
type goFile struct {
	rel    string
	file   *ast.File
	tagged bool
}

// TestIntegrationTreesAreTagged keeps the Docker-driving part of the harness and the suite out of
// the untagged build, which is what go test ./... in CI and the go-test-repo-mod hook run.
func TestIntegrationTreesAreTagged(t *testing.T) {
	t.Parallel()

	inTrees := 0

	for _, file := range parseGoFiles(t, "") {
		if !slices.ContainsFunc(integrationTrees, func(tree string) bool {
			return strings.HasPrefix(file.rel, tree)
		}) {
			continue
		}

		inTrees++

		if file.tagged {
			continue
		}

		for _, spec := range file.file.Imports {
			imported, _ := strconv.Unquote(spec.Path.Value)
			if first, _, _ := strings.Cut(imported, "/"); strings.Contains(first, ".") {
				t.Errorf("%s builds without the %s tag but imports %s", file.rel,
					integrationTag, imported)
			}
		}
	}

	if inTrees == 0 {
		t.Fatal("no Go file found in the integration trees; the check would pass on nothing")
	}
}

// TestUntaggedTestsAvoidDocker keeps Docker out of the untagged go test run: no untagged test file
// imports the harness or runs the docker CLI. Unit tests that talk to an httptest server through the
// Docker SDK client are fine; they need no daemon.
func TestUntaggedTestsAvoidDocker(t *testing.T) {
	t.Parallel()

	untagged := 0

	for _, file := range parseGoFiles(t, "_test.go") {
		if file.tagged {
			continue
		}

		untagged++

		for _, spec := range file.file.Imports {
			imported, _ := strconv.Unquote(spec.Path.Value)
			if imported == harnessPackage {
				t.Errorf("%s is untagged and imports %s", file.rel, imported)
			}
		}

		for _, call := range execCalls(file.file, "Command", "CommandContext") {
			if slices.ContainsFunc(call.Args, isLiteral("docker")) {
				t.Errorf("%s is untagged and runs docker", file.rel)
			}
		}

		// A constant would hide the literal from the exec check above.
		ast.Inspect(file.file, func(node ast.Node) bool {
			if value, ok := node.(*ast.ValueSpec); ok &&
				slices.ContainsFunc(value.Values, isLiteral("docker")) {
				t.Errorf("%s is untagged and names the docker binary in a declaration", file.rel)
			}

			return true
		})
	}

	if untagged == 0 {
		t.Fatal("no untagged test file found; the check would pass on nothing")
	}
}

// TestToolGateListIsClosed keeps the tools a meta-test may skip on to golangci-lint: toolgate's
// list is exactly that, every gated test names one of its constants, and no test looks a tool up
// on its own, which would be a skip CI cannot turn into a failure.
func TestToolGateListIsClosed(t *testing.T) {
	t.Parallel()

	if !slices.Equal(toolgate.Tools, []string{toolgate.GolangciLint}) {
		t.Errorf("toolgate.Tools = %v, want exactly golangci-lint", toolgate.Tools)
	}

	for _, file := range parseGoFiles(t, "_test.go") {
		if len(execCalls(file.file, "LookPath")) > 0 {
			t.Errorf("%s looks a tool up itself; gate it through toolgate.Lookup", file.rel)
		}

		ast.Inspect(file.file, func(node ast.Node) bool {
			call, ok := node.(*ast.CallExpr)
			if !ok || !isSelector(call.Fun, "toolgate", "Lookup") || len(call.Args) != 2 {
				return true
			}

			if selector, isSel := call.Args[1].(*ast.SelectorExpr); !isSel ||
				!isIdent(selector.X, "toolgate") {
				t.Errorf("%s gates on a tool that is not a toolgate constant", file.rel)
			}

			return true
		})
	}
}

// TestDockerCheckRecognizesItsTargets is the negative control of the checks above.
func TestDockerCheckRecognizesItsTargets(t *testing.T) {
	t.Parallel()

	file, err := parser.ParseFile(token.NewFileSet(), "sample_test.go", `package sample
import osexec "os/exec"
func run() { _ = osexec.Command("docker", "ps"); _, _ = osexec.LookPath("node") }
`, parser.ParseComments)
	if err != nil {
		t.Fatal(err)
	}

	if calls := execCalls(file, "Command", "CommandContext"); len(calls) != 1 ||
		!slices.ContainsFunc(calls[0].Args, isLiteral("docker")) {
		t.Error("exec.Command(\"docker\", ...) was not recognized")
	}

	if len(execCalls(file, "LookPath")) != 1 {
		t.Error("exec.LookPath was not recognized")
	}

	for constraintLine, tagged := range map[string]bool{
		"//go:build integration":                 true,
		"//go:build integration && linux":        true,
		"//go:build linux":                       false,
		"//go:build !windows":                    false,
		"//go:build !integration":                false,
		"//go:build integration || linux":        false,
		"//go:build (integration || e2e) && !js": false,
	} {
		sample, parseErr := parser.ParseFile(token.NewFileSet(), "sample_test.go",
			constraintLine+"\n\npackage sample\n", parser.ParseComments)
		if parseErr != nil {
			t.Fatal(parseErr)
		}

		if hasIntegrationTag(sample) != tagged {
			t.Errorf("%q: read as tagged %v, want %v", constraintLine, !tagged, tagged)
		}
	}

	if hasIntegrationTag(file) {
		t.Error("a file without a //go:build line was read as tagged")
	}
}

// parseGoFiles parses every Go file of the repository whose name ends in suffix.
func parseGoFiles(t *testing.T, suffix string) []goFile {
	t.Helper()

	root := toolgate.RepoRoot(t)
	fset := token.NewFileSet()

	var files []goFile

	err := filepath.WalkDir(root, func(path string, entry fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}

		if entry.IsDir() {
			switch entry.Name() {
			case ".git", "dist", "vendor", "testdata", ".cache":
				return filepath.SkipDir
			}

			return nil
		}

		if !strings.HasSuffix(path, ".go") || !strings.HasSuffix(path, suffix) {
			return nil
		}

		file, err := parser.ParseFile(fset, path, nil, parser.ParseComments)
		if err != nil {
			return fmt.Errorf("parse: %w", err)
		}

		rel, err := filepath.Rel(root, path)
		if err != nil {
			return fmt.Errorf("relative path of %s: %w", path, err)
		}

		files = append(
			files,
			goFile{rel: filepath.ToSlash(rel), file: file, tagged: hasIntegrationTag(file)},
		)

		return nil
	})
	if err != nil {
		t.Fatal(err)
	}

	return files
}

// hasIntegrationTag reports whether the file builds only with the integration tag: its //go:build
// constraint is false under every assignment of its other tags once integration is false. A file
// with no constraint, or one such as linux or !windows, is compiled by untagged go test somewhere.
func hasIntegrationTag(file *ast.File) bool {
	for _, group := range file.Comments {
		if group.Pos() > file.Package {
			break
		}

		for _, comment := range group.List {
			if !constraint.IsGoBuild(comment.Text) {
				continue
			}

			expr, err := constraint.Parse(comment.Text)
			if err != nil {
				return false
			}

			return !satisfiableWithoutIntegration(expr)
		}
	}

	return false
}

func satisfiableWithoutIntegration(expr constraint.Expr) bool {
	var others []string

	expr.Eval(func(tag string) bool {
		if tag != integrationTag && !slices.Contains(others, tag) {
			others = append(others, tag)
		}

		return false
	})

	for assignment := range 1 << len(others) {
		if expr.Eval(func(tag string) bool {
			index := slices.Index(others, tag)

			return index >= 0 && assignment&(1<<index) != 0
		}) {
			return true
		}
	}

	return false
}

// execCalls returns the calls to os/exec functions with the given names, under whatever name the
// file imports os/exec.
func execCalls(file *ast.File, names ...string) []*ast.CallExpr {
	local := ""

	for _, spec := range file.Imports {
		if spec.Path.Value != `"os/exec"` {
			continue
		}

		local = "exec"
		if spec.Name != nil {
			local = spec.Name.Name
		}
	}

	if local == "" {
		return nil
	}

	var calls []*ast.CallExpr

	ast.Inspect(file, func(node ast.Node) bool {
		call, ok := node.(*ast.CallExpr)
		if !ok {
			return true
		}

		for _, name := range names {
			if isSelector(call.Fun, local, name) {
				calls = append(calls, call)
			}
		}

		return true
	})

	return calls
}

func isSelector(expr ast.Expr, pkg, name string) bool {
	selector, ok := expr.(*ast.SelectorExpr)

	return ok && selector.Sel.Name == name && isIdent(selector.X, pkg)
}

func isIdent(expr ast.Expr, name string) bool {
	ident, ok := expr.(*ast.Ident)

	return ok && ident.Name == name
}

func isLiteral(value string) func(ast.Expr) bool {
	return func(expr ast.Expr) bool {
		literal, ok := expr.(*ast.BasicLit)
		if !ok || literal.Kind != token.STRING {
			return false
		}

		unquoted, err := strconv.Unquote(literal.Value)

		return err == nil && unquoted == value
	}
}
