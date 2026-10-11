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

package testenv

import (
	"bytes"
	"fmt"
	"log/slog"
	"slices"
	"strings"
	"testing"
	"time"
)

func TestParseOwner(t *testing.T) {
	t.Parallel()

	owner, err := ParseOwner("runner:fe80::1:4242")
	if err != nil || owner.Host != "runner:fe80::1" || owner.PID != 4242 {
		t.Errorf("ParseOwner kept %+v, %v; want the PID after the last colon", owner, err)
	}

	if (Owner{Host: "ci", PID: 7}).String() != "ci:7" {
		t.Error("an owner does not format as <hostname>:<pid>")
	}

	for _, bad := range []string{"", "host", ":12", "host:", "host:x", "host:0", "host:-3"} {
		_, err = ParseOwner(bad)
		if err == nil {
			t.Errorf("ParseOwner(%q) succeeded", bad)
		}
	}
}

func TestSweepRulesDecide(t *testing.T) {
	t.Parallel()

	now := time.Date(2026, 10, 11, 12, 0, 0, 0, time.UTC)
	rules := SweepRules{
		Now:        now,
		StaleAfter: time.Hour,
		Host:       "here",
		Self:       "self",
		Alive:      func(pid int) bool { return pid == 100 },
	}
	old := now.Add(-2 * time.Hour).Format(time.RFC3339)
	recent := now.Add(-10 * time.Minute).Format(time.RFC3339)

	for name, test := range map[string]struct {
		env    Found
		remove bool
		reason string
	}{
		"the current environment": {
			Found{ID: "self", Owner: "here:200", Started: old}, false, "current",
		},
		"recent, owner gone": {
			Found{ID: "a", Owner: "here:200", Started: recent}, false, "under",
		},
		"stale, owner running": {
			Found{ID: "a", Owner: "here:100", Started: old}, false, "still running",
		},
		"stale, another host": {
			Found{ID: "a", Owner: "there:200", Started: old}, false, "another host",
		},
		"stale, no owner label": {Found{ID: "a", Started: old}, false, "owner label"},
		"no started label":      {Found{ID: "a", Owner: "here:200"}, false, "started label"},
		"stale, owner gone": {
			Found{ID: "a", Owner: "here:200", Started: old}, true, "gone",
		},
	} {
		verdict := rules.Decide(test.env)
		if verdict.Remove != test.remove || !strings.Contains(verdict.Reason, test.reason) {
			t.Errorf("%s: remove=%v (%s), want remove=%v with %q", name, verdict.Remove,
				verdict.Reason, test.remove, test.reason)
		}
	}
}

// Not parallel: t.Setenv changes the process environment.
func TestStaleAfterFromEnv(t *testing.T) {
	t.Setenv(EnvStaleAfter, "")

	value, err := StaleAfterFromEnv()
	if err != nil || value != DefaultStaleAfter {
		t.Errorf("unset: %v, %v; want the default", value, err)
	}

	t.Setenv(EnvStaleAfter, "90m")

	value, err = StaleAfterFromEnv()
	if err != nil || value != 90*time.Minute {
		t.Errorf("90m: %v, %v", value, err)
	}

	for _, bad := range []string{"0s", "-1h", "soon", "10"} {
		t.Setenv(EnvStaleAfter, bad)

		_, err = StaleAfterFromEnv()
		if err == nil {
			t.Errorf("%q was accepted; a typo must not shorten the window", bad)
		}
	}
}

func TestRedactingLogger(t *testing.T) {
	t.Parallel()

	var out bytes.Buffer

	log := NewRedactingLogger(&out).With(slog.String("secret", "token-a"))
	log.Info(
		"bring-up",
		slog.Any("token", Secret("token-b")),
		slog.Any("env", []string{"DOCKER_AUTH=token-c", "token-d"}),
		slog.Any("nodes", []string{"manager-0", "worker-0"}),
	)

	logged := out.String()

	for _, leaked := range []string{"token-a", "token-b", "token-c", "token-d"} {
		if strings.Contains(logged, leaked) {
			t.Errorf("the log leaks %s:\n%s", leaked, logged)
		}
	}

	for _, kept := range []string{"DOCKER_AUTH=", "manager-0", "worker-0"} {
		if !strings.Contains(logged, kept) {
			t.Errorf("the log lost %s, which is not secret:\n%s", kept, logged)
		}
	}

	if formatted := fmt.Sprintf("%v %#v %s", Secret("x"), Secret("x"), Secret("x")); strings.
		Contains(formatted, "x\"") || strings.Count(formatted, redacted) != 3 {
		t.Errorf("a Secret formats as %q", formatted)
	}

	if got := RedactEnv([]string{"A=1", "B=", "C"}); !slices.Equal(
		got,
		[]string{"A=" + redacted, "B=" + redacted, redacted},
	) {
		t.Errorf("RedactEnv = %q", got)
	}
}
