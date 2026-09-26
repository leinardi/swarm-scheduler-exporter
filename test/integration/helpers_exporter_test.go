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
	"bytes"
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

const (
	exporterAttempts = 3
	// exporterReadyTimeout bounds one attempt: healthy, then metrics seeded.
	exporterReadyTimeout = 60 * time.Second
	// exporterStopTimeout is how long a SIGTERMed exporter gets before it is killed.
	exporterStopTimeout = 15 * time.Second
)

var (
	errExporterExited = errors.New("exporter exited")
	errStopTimeout    = errors.New("exporter did not exit")
)

// occupyFirstProbedPort is a test-only hook: when set, startExporter holds its first probed port
// with a dummy listener, so the first attempt loses the port race and the retry path runs.
var occupyFirstProbedPort bool

// startExporter runs the exporter binary against the test Swarm and returns its base URL once it
// is ready: /healthz answers 200, nodes_by_state accounts for every cluster node, and every
// service in services has desired_replicas and running_replicas series. Health alone is not
// enough — it can turn 200 before the event listener has seeded the metadata cache.
//
// The child gets -listen-addr on a probed free port, -poll-delay 1s and -log-format json, then
// args. The port can be taken between the probe and the child's bind; the exporter then exits
// non-zero, and that exit (like any other) is detected and retried on a new port, up to exporterAttempts times. The exporter is stopped
// with SIGTERM when the test ends, and its output is logged if the test failed.
func startExporter(t *testing.T, services []serviceKey, args ...string) string {
	t.Helper()

	ctx := testCtx(t)

	var (
		transcript strings.Builder
		current    *exporterProc
	)

	t.Cleanup(func() {
		if current != nil {
			stopCtx, cancel := teardownCtx()
			defer cancel()

			stopErr := current.stop(stopCtx)
			if stopErr != nil {
				t.Errorf("stop exporter: %v", stopErr)
			}

			transcript.WriteString(current.output.String())
		}

		if t.Failed() {
			t.Logf("exporter output:\n%s", transcript.String())
		}
	})

	for attempt := 1; attempt <= exporterAttempts; attempt++ {
		port, err := freePort(ctx)
		if err != nil {
			t.Fatal(err)
		}

		if attempt == 1 && occupyFirstProbedPort {
			blocker, listenErr := new(net.ListenConfig).Listen(ctx, "tcp", localAddr(port))
			if listenErr != nil {
				t.Fatalf("occupy probed port %d: %v", port, listenErr)
			}

			t.Cleanup(func() { _ = blocker.Close() })
		}

		current, err = launchExporter(ctx, port, args)
		if err != nil {
			t.Fatal(err)
		}

		fmt.Fprintf(&transcript, "--- attempt %d, port %d ---\n", attempt, port)

		baseURL := "http://" + localAddr(port)

		err = current.waitReady(ctx, baseURL, services)
		if err == nil {
			return baseURL
		}

		if !errors.Is(err, errExporterExited) {
			t.Fatalf("exporter not ready: %v", err)
		}

		t.Logf("exporter attempt %d on port %d failed, retrying: %v", attempt, port, err)

		stopErr := current.stop(ctx)
		if stopErr != nil {
			t.Fatalf("stop failed exporter attempt: %v", stopErr)
		}

		transcript.WriteString(current.output.String())
		current = nil
	}

	t.Fatalf("exporter did not start in %d attempts", exporterAttempts)

	return ""
}

// exporterEnv is the child's environment: the test process's own, minus every DOCKER_* variable,
// plus DOCKER_HOST pointing at the test Swarm's manager. The exporter builds its client with
// client.FromEnv, so an inherited DOCKER_TLS_VERIFY, DOCKER_CERT_PATH, DOCKER_API_VERSION or
// DOCKER_CONTEXT from the developer's shell would otherwise leak into it.
func exporterEnv() []string {
	env := make([]string, 0, len(os.Environ())+1)

	for _, entry := range os.Environ() {
		if strings.HasPrefix(entry, "DOCKER_") {
			continue
		}

		env = append(env, entry)
	}

	return append(env, "DOCKER_HOST="+cluster.Manager.DockerHost)
}

func freePort(ctx context.Context) (int, error) {
	listener, err := new(net.ListenConfig).Listen(ctx, "tcp", "127.0.0.1:0")
	if err != nil {
		return 0, fmt.Errorf("probe free port: %w", err)
	}

	port := listener.Addr().(*net.TCPAddr).Port //nolint:forcetypeassert // a tcp listener's address is always *net.TCPAddr

	err = listener.Close()
	if err != nil {
		return 0, fmt.Errorf("release probed port: %w", err)
	}

	return port, nil
}

func localAddr(port int) string {
	return net.JoinHostPort("127.0.0.1", strconv.Itoa(port))
}

// exporterProc is one running exporter child.
type exporterProc struct {
	cmd    *exec.Cmd
	output *outputWatcher
	exited chan struct{} // closed once cmd.Wait has returned
}

func launchExporter(ctx context.Context, port int, args []string) (*exporterProc, error) {
	cmdArgs := append(
		[]string{"-listen-addr", localAddr(port), "-poll-delay", "1s", "-log-format", "json"},
		args...)

	// Detached from ctx: the test's context ends before its cleanups run, and exec would then kill
	// the exporter outright. stop ends it instead, with SIGTERM, from the cleanup.
	cmd := exec.CommandContext(context.WithoutCancel(ctx), exporterBinary, cmdArgs...)
	cmd.Env = exporterEnv()

	// The exporter's logger writes to stdout and a crash lands on stderr: both go through the
	// same watcher, which exec serializes when the writer is shared.
	watcher := newOutputWatcher()
	cmd.Stdout = watcher
	cmd.Stderr = watcher

	err := cmd.Start()
	if err != nil {
		return nil, fmt.Errorf("start exporter: %w", err)
	}

	proc := &exporterProc{cmd: cmd, output: watcher, exited: make(chan struct{})}

	go func() {
		waitErr := cmd.Wait()
		fmt.Fprintf(watcher, "--- exited: %v ---\n", waitErr)
		close(proc.exited)
	}()

	return proc, nil
}

// waitReady waits for the exporter to be healthy and seeded. It returns errExporterExited, which
// the caller retries, as soon as the exporter exits (a lost port race ends that way too).
func (p *exporterProc) waitReady(ctx context.Context, baseURL string, services []serviceKey) error {
	ticker := time.NewTicker(pollInterval)
	defer ticker.Stop()

	deadline := time.NewTimer(exporterReadyTimeout)
	defer deadline.Stop()

	for {
		lastErr := p.readiness(ctx, baseURL, services)

		// A foreign process that grabbed the port could answer, but it cannot have served seeded
		// exporter metrics before our exporter, failing to bind, exited: checking the exit after
		// a successful probe settles whose metrics those were.
		select {
		case <-p.exited:
			return errExporterExited
		default:
		}

		if lastErr == nil {
			return nil
		}

		select {
		case <-p.exited:
			return errExporterExited
		case <-ctx.Done():
			return fmt.Errorf("wait for exporter: %w: %w", ctx.Err(), lastErr)
		case <-deadline.C:
			return fmt.Errorf("exporter not ready within %s: %w", exporterReadyTimeout, lastErr)
		case <-ticker.C:
		}
	}
}

func (*exporterProc) readiness(ctx context.Context, baseURL string, services []serviceKey) error {
	_, status, err := httpGet(ctx, baseURL+"/healthz")
	if err != nil {
		return err
	}

	if status != http.StatusOK {
		return fmt.Errorf("/healthz answered %d: %w", status, errNotYet)
	}

	scraped, err := scrapeMetrics(ctx, baseURL)
	if err != nil {
		return err
	}

	nodes := scraped.sum(metricNodesByState, nil)
	if nodes != float64(len(cluster.Nodes)) {
		return fmt.Errorf(
			"%s sums to %g, want %d: %w",
			metricNodesByState,
			nodes,
			len(cluster.Nodes),
			errNotYet,
		)
	}

	for _, svc := range services {
		if !scraped.hasService(metricDesired, svc) || !scraped.hasService(metricRunning, svc) {
			return fmt.Errorf(
				"no desired/running series yet for %s/%s: %w",
				svc.stack,
				svc.service,
				errNotYet,
			)
		}
	}

	return nil
}

// stop sends SIGTERM and waits for the exporter to exit, killing it if it does not in time.
func (p *exporterProc) stop(ctx context.Context) error {
	select {
	case <-p.exited:
		return nil
	default:
	}

	_ = p.cmd.Process.Signal(syscall.SIGTERM)

	timer := time.NewTimer(exporterStopTimeout)
	defer timer.Stop()

	select {
	case <-p.exited:
		return nil
	case <-timer.C:
	case <-ctx.Done():
	}

	_ = p.cmd.Process.Kill()
	<-p.exited

	return fmt.Errorf("%w within %s of SIGTERM; killed", errStopTimeout, exporterStopTimeout)
}

// outputWatcher collects the child's output. The mutex orders exec's writes against String, which
// the test goroutine calls while the child is still running.
type outputWatcher struct {
	mu  sync.Mutex
	all bytes.Buffer
}

func newOutputWatcher() *outputWatcher {
	return &outputWatcher{}
}

func (w *outputWatcher) Write(chunk []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()

	w.all.Write(chunk)

	return len(chunk), nil
}

func (w *outputWatcher) String() string {
	w.mu.Lock()
	defer w.mu.Unlock()

	return w.all.String()
}
