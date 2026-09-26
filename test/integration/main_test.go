//go:build integration

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

// Package integration_test runs the exporter binary against a throwaway multi-node Swarm made of
// DinD containers (internal/testenv) and checks what it exposes on /metrics. Tests run one after
// another — they share the cluster and several of them disrupt its nodes — so none of them calls
// t.Parallel. Each run gets a fresh cluster, which is why stack names can be fixed.
package integration_test

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"syscall"
	"testing"
	"time"

	dockerclient "github.com/moby/moby/client"

	"github.com/leinardi/swarm-scheduler-exporter/internal/testenv"
)

const (
	envSuiteTimeout     = "SSE_IT_SUITE_TIMEOUT"
	defaultSuiteTimeout = 12 * time.Minute

	// teardownTimeout bounds every teardown step (cluster Down, t.Cleanup removals, the exporter's
	// SIGTERM wait). Teardown runs on its own context so it still runs after the suite deadline;
	// the Make target's go test -timeout exceeds the suite deadline plus this, so teardown ends
	// before go test's hard timeout panics.
	teardownTimeout = 2 * time.Minute

	// dockerOpTimeout bounds a single Docker call made by a test.
	dockerOpTimeout = 30 * time.Second
)

var (
	// suiteCtx expires at the suite deadline (or on SIGINT/SIGTERM). testing.M.Run takes no
	// context, so the deadline only works because every wait in the tests derives from it through
	// testCtx.
	suiteCtx context.Context

	// cluster is the shared test Swarm, brought up once by TestMain.
	cluster *testenv.Cluster

	// swarmClient talks to the test Swarm's manager.
	swarmClient *dockerclient.Client

	// exporterBinary is the exporter under test (SSE_IT_BINARY).
	exporterBinary string
)

func TestMain(m *testing.M) {
	os.Exit(runSuite(m))
}

func runSuite(m *testing.M) int {
	suiteTimeout := defaultSuiteTimeout

	rawTimeout := os.Getenv(envSuiteTimeout)
	if rawTimeout != "" {
		parsed, err := time.ParseDuration(rawTimeout)
		if err != nil {
			fmt.Fprintf(os.Stderr, "[it] parse %s: %v\n", envSuiteTimeout, err)

			return 1
		}

		suiteTimeout = parsed
	}

	deadlineCtx, cancelDeadline := context.WithTimeout(context.Background(), suiteTimeout)
	defer cancelDeadline()

	// Ctrl-C cancels the suite instead of killing it, so the waits return and teardown runs.
	var stopSignals context.CancelFunc

	//nolint:fatcontext // suiteCtx is the package-level suite context, set once here on purpose
	suiteCtx, stopSignals = signal.NotifyContext(deadlineCtx, os.Interrupt, syscall.SIGTERM)
	defer stopSignals()

	spec, err := testenv.SpecFromEnv()
	if err != nil {
		fmt.Fprintf(os.Stderr, "[it] spec from env: %v\n", err)

		return 1
	}

	exporterBinary = spec.Binary

	_, err = os.Stat(exporterBinary)
	if exporterBinary == "" || err != nil {
		fmt.Fprintf(
			os.Stderr,
			"[it] %s must name the built exporter binary (make go-test-integration sets it): %q\n",
			testenv.EnvBinary,
			exporterBinary,
		)

		return 1
	}

	started := time.Now()

	cluster, err = testenv.Up(suiteCtx, spec)
	if err != nil {
		fmt.Fprintf(os.Stderr, "[it] cluster up: %v\n", err)

		return 1
	}

	fmt.Fprintf(os.Stderr, "[it] cluster %s up in %s (%d nodes)\n",
		spec.EnvID, time.Since(started).Round(time.Millisecond), len(cluster.Nodes))

	code := runWithCluster(m)

	if code != 0 && spec.KeepOnFailure {
		fmt.Fprintf(
			os.Stderr,
			"[it] tests failed; keeping cluster %s (manager %s). Clean up with:\n  %s\n",
			spec.EnvID,
			cluster.Manager.DockerHost,
			testenv.CleanupCommand(spec.EnvID),
		)

		return code
	}

	teardownCtx, cancelTeardown := context.WithTimeout(context.Background(), teardownTimeout)
	defer cancelTeardown()

	err = cluster.Down(teardownCtx)
	if err != nil {
		fmt.Fprintf(
			os.Stderr,
			"[it] cluster down: %v\nClean up with:\n  %s\n",
			err,
			testenv.CleanupCommand(spec.EnvID),
		)

		if code == 0 {
			code = 1
		}
	}

	return code
}

func runWithCluster(m *testing.M) int {
	var err error

	swarmClient, err = cluster.Manager.Client()
	if err != nil {
		fmt.Fprintf(os.Stderr, "[it] manager client: %v\n", err)

		return 1
	}
	defer swarmClient.Close()

	return m.Run()
}

// testCtx returns a context that ends with the test or at the suite deadline, whichever comes
// first. Every Docker call and every wait in a test derives from it, so once the deadline passes,
// blocked tests fail fast and m.Run returns in time for teardown.
func testCtx(t *testing.T) context.Context {
	t.Helper()

	ctx, cancel := context.WithCancel(t.Context())
	stop := context.AfterFunc(suiteCtx, cancel)

	t.Cleanup(func() {
		stop()
		cancel()
	})

	return ctx
}

// teardownCtx returns a context for cleanup work. It deliberately does not derive from the test
// or suite context: those are already done when cleanup runs, and cleanup must still happen.
func teardownCtx() (context.Context, context.CancelFunc) {
	return context.WithTimeout(context.Background(), teardownTimeout)
}

// opCtx bounds one Docker call made from a test.
func opCtx(ctx context.Context) (context.Context, context.CancelFunc) {
	return context.WithTimeout(ctx, dockerOpTimeout)
}
