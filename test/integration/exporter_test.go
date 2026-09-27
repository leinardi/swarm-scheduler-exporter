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

package integration_test

import (
	"context"
	"errors"
	"fmt"
	"os/exec"
	"strings"
	"testing"
	"time"
)

// TestExporter_PortRetry makes the first port startExporter probes unavailable: the exporter's
// bind fails, and startExporter must notice and succeed on a new port.
func TestExporter_PortRetry(t *testing.T) {
	occupyFirstProbedPort = true

	t.Cleanup(func() { occupyFirstProbedPort = false })

	baseURL := startExporter(t, nil)

	eventually(t, 30*time.Second, metricsMatch(baseURL, "", want(metricHealth, nil, 1)))
}

// TestExporter_HealthcheckHealthy runs the binary in -healthcheck mode against a ready exporter:
// the probe must exit 0.
func TestExporter_HealthcheckHealthy(t *testing.T) {
	baseURL := startExporter(t, nil)

	eventually(t, 30*time.Second, func(ctx context.Context) error {
		exitCode, output, err := runHealthcheckProbe(ctx, baseURL)
		if err != nil {
			return err
		}

		if exitCode != 0 {
			return fmt.Errorf("-healthcheck exited %d (%q): %w", exitCode, output, errNotYet)
		}

		return nil
	})
}

// TestExporter_HealthcheckUnhealthy runs the probe against an exporter whose service list keeps
// failing: it must exit 1 and print why.
func TestExporter_HealthcheckUnhealthy(t *testing.T) {
	proxy := startDockerProxy(t)
	proxy.setServiceListFailing(true)

	baseURL := startExporterServing(t, proxy.dockerHost())

	exitCode, output, err := runHealthcheckProbe(testCtx(t), baseURL)
	if err != nil {
		t.Fatal(err)
	}

	if exitCode != 1 || !strings.Contains(output, "initial resync not completed") {
		t.Errorf(
			"-healthcheck during a failing seed exited %d (%q), want 1 and \"initial resync not completed\"",
			exitCode,
			output,
		)
	}
}

// runHealthcheckProbe runs the exporter binary with -healthcheck against the exporter serving at
// baseURL, and returns its exit code and output. err is set only when the probe could not run.
func runHealthcheckProbe(
	ctx context.Context,
	baseURL string,
) (exitCode int, output string, err error) {
	probeArgs := []string{"-healthcheck", "-listen-addr", strings.TrimPrefix(baseURL, "http://")}

	combined, runErr := exec.CommandContext(ctx, exporterBinary, probeArgs...).CombinedOutput()
	if runErr == nil {
		return 0, string(combined), nil
	}

	if exitErr, ok := errors.AsType[*exec.ExitError](runErr); ok {
		return exitErr.ExitCode(), string(combined), nil
	}

	return 0, string(combined), fmt.Errorf("run -healthcheck: %w", runErr)
}
