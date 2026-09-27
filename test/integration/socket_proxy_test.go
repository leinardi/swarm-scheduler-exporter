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
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"maps"
	"net/http"
	"os"
	"regexp"
	"slices"
	"strings"
	"testing"
	"time"

	"github.com/moby/moby/api/pkg/stdcopy"
	"github.com/moby/moby/api/types/container"
	"github.com/moby/moby/api/types/swarm"
	dockerclient "github.com/moby/moby/client"

	"github.com/leinardi/swarm-scheduler-exporter/internal/testenv"
)

const (
	// socketProxyComposeFile and socketProxyEnvFile are the example this test pins, relative to
	// this package's directory.
	socketProxyComposeFile = "../../deployments/docker/docker-compose.socket-proxy.yaml"
	socketProxyEnvFile     = "../../deployments/docker/socket-proxy.env"
)

var (
	// socketProxyImageLine matches the proxy's image line in the compose file.
	socketProxyImageLine = regexp.MustCompile(
		`(?m)^\s+image:\s*(ghcr\.io/tecnativa/docker-socket-proxy:\S+)`,
	)

	// socketProxyEnvName is the shape of a variable name in socket-proxy.env.
	socketProxyEnvName = regexp.MustCompile(`^[A-Z][A-Z0-9_]*$`)

	// socketProxySettings are the variables of the proxy image's environment that configure
	// HAProxy rather than open an API section. HAPROXY_* (the base image's build metadata) is
	// skipped too; everything else in the image's environment is an ACL variable.
	socketProxySettings = map[string]bool{
		"BIND_CONFIG":  true,
		"DISABLE_IPV6": true,
		"LOG_LEVEL":    true,
		"PATH":         true,
		"SOCKET_PATH":  true,
	}
)

// TestSocketProxy_ExampleAllowlist runs the real, pinned tecnativa/docker-socket-proxy image with
// the example's socket-proxy.env in front of the test Swarm's manager: the exporter must work
// through it, what the allowlist leaves off must be refused, and the breadth the README states
// for CONTAINERS=1 must hold. The in-process dockerProxy of the other tests forwards everything,
// so it says nothing about this ACL.
func TestSocketProxy_ExampleAllowlist(t *testing.T) {
	imageRef := socketProxyImage(t)
	allowlist := readSocketProxyEnv(t)

	err := cluster.EnsureHostImage(testCtx(t), imageRef)
	if err != nil {
		t.Fatal(err)
	}

	assertAllowlistComplete(t, imageRef, allowlist)

	proxy := startSocketProxy(t, imageRef, allowlist)

	t.Run("denied", func(t *testing.T) {
		// A write is refused whatever the allowlist says: the exporter is read-only, so POST=1 is
		// never a deliberate change to this example. A read is refused unless its section is 1 in
		// socket-proxy.env, so enabling one on purpose changes the expectation there, in one place.
		requests := []struct {
			method string
			path   string
			// section is the socket-proxy.env variable that opens a read; empty for a write.
			section string
		}{
			{method: http.MethodPost, path: "/services/create"},
			{method: http.MethodDelete, path: "/containers/x"},
			{method: http.MethodGet, path: "/secrets", section: "SECRETS"},
			{method: http.MethodGet, path: "/volumes", section: "VOLUMES"},
			{method: http.MethodGet, path: "/images/json", section: "IMAGES"},
			{method: http.MethodGet, path: "/configs", section: "CONFIGS"},
			{method: http.MethodGet, path: "/swarm", section: "SWARM"},
			// The image enables VERSION by default: these catch that default leaking through.
			{method: http.MethodGet, path: "/version", section: "VERSION"},
			{method: http.MethodGet, path: "/info", section: "INFO"},
		}

		for _, request := range requests {
			status, _ := proxy.request(t, request.method, request.path)

			switch {
			case request.section == "":
				if status != http.StatusForbidden {
					t.Errorf(
						"%s %s = %d, want 403: the exporter never writes, so POST must stay 0",
						request.method,
						request.path,
						status,
					)
				}
			case allowlist[request.section] != "1":
				if status != http.StatusForbidden {
					t.Errorf(
						"%s %s = %d, want 403: %s is not 1 in socket-proxy.env",
						request.method,
						request.path,
						status,
						request.section,
					)
				}
			case status == http.StatusForbidden:
				t.Errorf(
					"%s %s = 403, want it forwarded: %s=1 in socket-proxy.env",
					request.method,
					request.path,
					request.section,
				)
			}
		}
	})

	// The breadth the README states: every enabled section forwards every GET under its prefix,
	// logs included. The exporter runs as a host process, so there is no exporter container to
	// read from: IDs that do not exist show whether the proxy forwards the request (a 404 from the
	// daemon) or refuses it (its own 403).
	t.Run("scope", func(t *testing.T) {
		for _, path := range []string{
			// stdout=1: without a stream the daemon answers 400 before looking the object up.
			"/services/does-not-exist/logs?stdout=1",
			"/tasks/does-not-exist/logs?stdout=1",
			"/containers/does-not-exist/logs?stdout=1",
			"/containers/does-not-exist/archive?path=/",
		} {
			status, body := proxy.request(t, http.MethodGet, path)

			var daemonError struct {
				Message string `json:"message"`
			}

			decodeErr := json.Unmarshal(body, &daemonError)
			if status != http.StatusNotFound || decodeErr != nil || daemonError.Message == "" {
				t.Errorf(
					"GET %s = %d %q, want the daemon's 404 with a JSON message: the proxy "+
						"forwards every GET under an enabled section, as the README states",
					path,
					status,
					body,
				)
			}
		}

		if status, body := proxy.request(
			t,
			http.MethodGet,
			"/containers/json",
		); status != http.StatusOK {
			t.Errorf("GET /containers/json = %d %q, want 200", status, body)
		}
	})

	t.Run("exporter", func(t *testing.T) {
		const stack = "it-socket-proxy"

		web := serviceKey{stack: stack, service: "web"}
		serviceID := deployService(t, stack, web.service, &serviceOpts{replicas: 1})

		baseURL := startExporterVia(t, "tcp://"+proxy.address, []serviceKey{web}, "-containers")

		scraped, err := scrapeMetrics(testCtx(t), baseURL)
		if err != nil {
			t.Fatal(err)
		}

		reconnectsBefore := scraped.sum(metricEventsReconnects, nil)

		// A restart drops the event stream, as HAProxy's idle timeout would.
		proxy.restart(t)

		eventually(t, 60*time.Second, func(ctx context.Context) error {
			scraped, err := scrapeMetrics(ctx, baseURL)
			if err != nil {
				return err
			}

			reconnects := scraped.sum(metricEventsReconnects, nil)
			if reconnects <= reconnectsBefore {
				return fmt.Errorf("%s = %g, want above %g: %w",
					metricEventsReconnects, reconnects, reconnectsBefore, errNotYet)
			}

			return nil
		})

		eventually(t, 60*time.Second, metricsMatch(baseURL, "", want(metricHealth, nil, 1)))

		// The reconnected stream carries events again: a scale-up reaches desired_replicas.
		updateService(t, serviceID, func(spec *swarm.ServiceSpec) {
			replicas := uint64(2)
			spec.Mode.Replicated.Replicas = &replicas
		})

		eventually(
			t,
			30*time.Second,
			metricsMatch(baseURL, stack, wantService(metricDesired, web, 2)),
		)
	})
}

// socketProxyImage returns the proxy image the example compose file pins.
func socketProxyImage(t *testing.T) string {
	t.Helper()

	compose, err := os.ReadFile(socketProxyComposeFile)
	if err != nil {
		t.Fatalf("read %s: %v", socketProxyComposeFile, err)
	}

	match := socketProxyImageLine.FindSubmatch(compose)
	if match == nil {
		t.Fatalf("%s has no docker-socket-proxy image line", socketProxyComposeFile)
	}

	return string(match[1])
}

// readSocketProxyEnv parses socket-proxy.env: KEY=VALUE lines, blank lines and # comments, each
// value 0 or 1, no key twice.
func readSocketProxyEnv(t *testing.T) map[string]string {
	t.Helper()

	file, err := os.Open(socketProxyEnvFile)
	if err != nil {
		t.Fatalf("open %s: %v", socketProxyEnvFile, err)
	}
	defer file.Close()

	allowlist := make(map[string]string)
	scanner := bufio.NewScanner(file)

	for lineNumber := 1; scanner.Scan(); lineNumber++ {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		name, value, found := strings.Cut(line, "=")
		if !found || !socketProxyEnvName.MatchString(name) || (value != "0" && value != "1") {
			t.Fatalf("%s:%d: want NAME=0 or NAME=1, have %q", socketProxyEnvFile, lineNumber, line)
		}

		if _, duplicate := allowlist[name]; duplicate {
			t.Fatalf("%s:%d: %s set twice", socketProxyEnvFile, lineNumber, name)
		}

		allowlist[name] = value
	}

	err = scanner.Err()
	if err != nil {
		t.Fatalf("read %s: %v", socketProxyEnvFile, err)
	}

	return allowlist
}

// assertAllowlistComplete fails the test if the pinned image defines an ACL variable that
// socket-proxy.env does not set: a new default-on section would otherwise widen access silently.
func assertAllowlistComplete(t *testing.T, imageRef string, allowlist map[string]string) {
	t.Helper()

	callCtx, cancel := opCtx(testCtx(t))
	defer cancel()

	inspected, err := cluster.HostClient().ImageInspect(callCtx, imageRef)
	if err != nil {
		t.Fatalf("inspect %s: %v", imageRef, err)
	}

	if inspected.Config == nil {
		t.Fatalf("inspect %s: no image config", imageRef)
	}

	for _, entry := range inspected.Config.Env {
		name, _, _ := strings.Cut(entry, "=")
		if socketProxySettings[name] || strings.HasPrefix(name, "HAPROXY_") {
			continue
		}

		if _, set := allowlist[name]; !set {
			t.Errorf(
				"%s does not set %s, which the image defaults to %q: set it explicitly",
				socketProxyEnvFile,
				name,
				entry,
			)
		}
	}
}

// socketProxy is the proxy container this test runs on the host daemon.
type socketProxy struct {
	containerID string
	// address is the host:port the proxy listens on, on the host's loopback.
	address string
}

// startSocketProxy runs imageRef on the host daemon with the allowlist, in front of the test
// Swarm's manager, and waits until it answers /_ping. The container is removed when the test
// ends, and its output logged if the test failed.
func startSocketProxy(t *testing.T, imageRef string, allowlist map[string]string) *socketProxy {
	t.Helper()

	ctx := testCtx(t)
	hostClient := cluster.HostClient()

	port, err := freePort(ctx)
	if err != nil {
		t.Fatal(err)
	}

	proxy := &socketProxy{address: localAddr(port)}

	env := make([]string, 0, len(allowlist)+2)
	for _, name := range slices.Sorted(maps.Keys(allowlist)) {
		env = append(env, name+"="+allowlist[name])
	}

	// Host networking reaches the manager's published daemon port on loopback. HAProxy's server
	// line takes host:port as well as a socket path.
	env = append(env,
		"SOCKET_PATH="+strings.TrimPrefix(cluster.Manager.DockerHost, "tcp://"),
		"BIND_CONFIG="+proxy.address,
	)

	callCtx, cancel := opCtx(ctx)
	defer cancel()

	created, err := hostClient.ContainerCreate(callCtx, dockerclient.ContainerCreateOptions{
		Config: &container.Config{
			Image:  imageRef,
			Env:    env,
			Labels: map[string]string{testenv.LabelEnvID: cluster.Spec.EnvID},
		},
		HostConfig: &container.HostConfig{
			NetworkMode: "host",
			CapDrop:     []string{"ALL"},
		},
		Name: testenv.ResourceName(cluster.Spec.EnvID, "socket-proxy"),
	})
	if err != nil {
		t.Fatalf("create socket proxy: %v", err)
	}

	proxy.containerID = created.ID

	t.Cleanup(func() {
		stopCtx, cancelStop := teardownCtx()
		defer cancelStop()

		if t.Failed() {
			t.Logf("socket proxy output:\n%s", proxy.output(stopCtx))
		}

		_, removeErr := hostClient.ContainerRemove(
			stopCtx,
			proxy.containerID,
			dockerclient.ContainerRemoveOptions{Force: true},
		)
		if removeErr != nil {
			t.Errorf("remove socket proxy: %v", removeErr)
		}
	})

	_, err = hostClient.ContainerStart(
		callCtx,
		proxy.containerID,
		dockerclient.ContainerStartOptions{},
	)
	if err != nil {
		t.Fatalf("start socket proxy: %v", err)
	}

	proxy.waitPing(t)

	return proxy
}

// waitPing waits until the proxy answers GET /_ping with 200, which also proves it reaches the
// manager.
func (p *socketProxy) waitPing(t *testing.T) {
	t.Helper()

	eventually(t, 30*time.Second, func(ctx context.Context) error {
		_, status, err := httpGet(ctx, "http://"+p.address+"/_ping")
		if err != nil {
			return fmt.Errorf("%w: %w", err, errNotYet)
		}

		if status != http.StatusOK {
			return fmt.Errorf("GET /_ping = %d: %w", status, errNotYet)
		}

		return nil
	})
}

// request sends method path straight to the proxy and returns the status and body.
func (p *socketProxy) request(t *testing.T, method, path string) (status int, body []byte) {
	t.Helper()

	callCtx, cancel := context.WithTimeout(testCtx(t), httpTimeout)
	defer cancel()

	request, err := http.NewRequestWithContext(
		callCtx,
		method,
		"http://"+p.address+path,
		http.NoBody,
	)
	if err != nil {
		t.Fatalf("build %s %s: %v", method, path, err)
	}

	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatalf("%s %s: %v", method, path, err)
	}
	defer response.Body.Close()

	body, err = io.ReadAll(response.Body)
	if err != nil {
		t.Fatalf("read %s %s: %v", method, path, err)
	}

	return response.StatusCode, body
}

// restart restarts the proxy container and waits until it answers again.
func (p *socketProxy) restart(t *testing.T) {
	t.Helper()

	callCtx, cancel := opCtx(testCtx(t))
	defer cancel()

	_, err := cluster.HostClient().ContainerRestart(
		callCtx,
		p.containerID,
		dockerclient.ContainerRestartOptions{},
	)
	if err != nil {
		t.Fatalf("restart socket proxy: %v", err)
	}

	p.waitPing(t)
}

// output returns the proxy container's stdout and stderr, or why it could not be read.
func (p *socketProxy) output(ctx context.Context) string {
	logs, err := cluster.HostClient().
		ContainerLogs(ctx, p.containerID, dockerclient.ContainerLogsOptions{
			ShowStdout: true,
			ShowStderr: true,
		})
	if err != nil {
		return fmt.Sprintf("(read logs: %v)", err)
	}
	defer logs.Close()

	var combined strings.Builder

	_, err = stdcopy.StdCopy(&combined, &combined, logs)
	if err != nil {
		fmt.Fprintf(&combined, "\n(read logs: %v)", err)
	}

	return combined.String()
}
