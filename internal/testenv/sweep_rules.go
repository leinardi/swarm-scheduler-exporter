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

// The sweep's decision rules and the owner label, kept free of the Docker SDK and of the
// integration build tag so their tests run in the untagged go test, with no daemon.

import (
	"errors"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"
)

const (
	// LabelOwner is "<hostname>:<pid>" of the go test process that created the resource.
	LabelOwner = "swarm-scheduler-exporter.it.owner"
	// LabelStarted is when the environment came up, in RFC 3339.
	LabelStarted = "swarm-scheduler-exporter.it.started"
	// LabelPIDNamespace is the PID namespace of the owner ("pid:[4026531836]"): a PID means
	// something only inside its namespace, and a test in a container that shares the host's
	// hostname (--network host) has its own.
	LabelPIDNamespace = "swarm-scheduler-exporter.it.pidns"

	// EnvStaleAfter overrides how old an abandoned environment must be before the sweep removes it.
	EnvStaleAfter = "SSE_IT_STALE_AFTER"
	// DefaultStaleAfter is the age below which no other environment is ever removed.
	DefaultStaleAfter = time.Hour
)

var (
	errBadOwner      = errors.New("owner is not <hostname>:<pid>")
	errBadStaleAfter = errors.New("bad stale-after window")
)

// Owner is the process that created an environment.
type Owner struct {
	Host string
	PID  int
	// PIDNamespace is the namespace PID lives in, empty where it cannot be read.
	PIDNamespace string
}

// String formats the owner as the LabelOwner value; the namespace has a label of its own.
func (o Owner) String() string {
	return o.Host + ":" + strconv.Itoa(o.PID)
}

// CurrentOwner returns this process on this host.
func CurrentOwner() (Owner, error) {
	host, err := os.Hostname()
	if err != nil {
		return Owner{}, fmt.Errorf("hostname: %w", err)
	}

	return Owner{Host: host, PID: os.Getpid(), PIDNamespace: CurrentPIDNamespace()}, nil
}

// CurrentPIDNamespace returns this process's PID namespace, or "" where /proc does not say. An
// empty namespace never matches, so an environment from such a process is never swept by PID.
func CurrentPIDNamespace() string {
	namespace, err := os.Readlink("/proc/self/ns/pid")
	if err != nil {
		return ""
	}

	return namespace
}

// ParseOwner reads an owner label value. The PID is after the last colon, so a hostname may hold
// colons of its own.
func ParseOwner(value string) (Owner, error) {
	colon := strings.LastIndex(value, ":")
	if colon <= 0 {
		return Owner{}, fmt.Errorf("%w: %q", errBadOwner, value)
	}

	pid, err := strconv.Atoi(value[colon+1:])
	if err != nil || pid <= 0 {
		return Owner{}, fmt.Errorf("%w: %q", errBadOwner, value)
	}

	return Owner{Host: value[:colon], PID: pid}, nil
}

// Found is one environment as its labels describe it. The owner and started labels are taken from
// whichever of its resources carries them.
type Found struct {
	ID           string
	Owner        string
	Started      string
	PIDNamespace string
}

// Verdict is the sweep's decision about one environment, with the reason a reader is shown.
type Verdict struct {
	Env    Found
	Remove bool
	Reason string
}

// SweepRules decides which environments are abandoned. Another envid is removed only when all of
// these hold: it is older than StaleAfter, and its owner is provably dead -- on this host, in this
// PID namespace, and its PID does not exist. A foreign host is never assumed dead, and a label that cannot be read proves
// nothing: such an environment is left for make sweep-test-leaks.
type SweepRules struct {
	Now        time.Time
	StaleAfter time.Duration
	Host       string
	// PIDNamespace is this process's PID namespace; empty matches nothing.
	PIDNamespace string
	// Self is the current envid, which a sweep never touches; Down owns it.
	Self string
	// Alive reports whether a PID exists on this host.
	Alive func(pid int) bool
}

// Decide applies the rules to one environment.
func (r *SweepRules) Decide(env Found) Verdict {
	keep := func(reason string) Verdict { return Verdict{Env: env, Remove: false, Reason: reason} }

	if env.ID == r.Self {
		return keep("the current environment")
	}

	started, err := time.Parse(time.RFC3339, env.Started)
	if err != nil {
		return keep("its started label is unreadable")
	}

	if age := r.Now.Sub(started); age < r.StaleAfter {
		return keep(fmt.Sprintf("started %s ago, under %s", age.Round(time.Minute), r.StaleAfter))
	}

	owner, err := ParseOwner(env.Owner)
	if err != nil {
		return keep("its owner label is unreadable")
	}

	if owner.Host != r.Host {
		return keep("owned by another host, " + owner.Host)
	}

	if r.PIDNamespace == "" || env.PIDNamespace != r.PIDNamespace {
		return keep("owned by a process in another or unknown PID namespace")
	}

	if r.Alive(owner.PID) {
		return keep(fmt.Sprintf("its owner, PID %d, is still running", owner.PID))
	}

	return Verdict{
		Env:    env,
		Remove: true,
		Reason: fmt.Sprintf("stale, and its owner PID %d is gone", owner.PID),
	}
}

// StaleAfterFromEnv reads SSE_IT_STALE_AFTER, defaulting to DefaultStaleAfter. A value that is not
// a positive duration is an error rather than a default: a typo must not shorten the window.
func StaleAfterFromEnv() (time.Duration, error) {
	raw := os.Getenv(EnvStaleAfter)
	if raw == "" {
		return DefaultStaleAfter, nil
	}

	value, err := time.ParseDuration(raw)
	if err != nil || value <= 0 {
		return 0, fmt.Errorf(
			"%w: %s=%q is not a positive duration",
			errBadStaleAfter,
			EnvStaleAfter,
			raw,
		)
	}

	return value, nil
}
