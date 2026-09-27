/*
 * MIT License
 *
 * Copyright (c) 2025 Roberto Leinardi
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

package main

import (
	"bytes"
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/moby/moby/client"

	"github.com/leinardi/swarm-scheduler-exporter/internal/collector"
	"github.com/leinardi/swarm-scheduler-exporter/internal/server"
)

func TestStringSlice_Set_EmptyErrors(t *testing.T) {
	var s stringSlice

	err := s.Set("")
	if !errors.Is(err, ErrEmptyFlagValue) {
		t.Errorf("expected ErrEmptyFlagValue, got %v", err)
	}

	if len(s) != 0 {
		t.Error("empty value must not be appended")
	}
}

func TestStringSlice_Set_Appends(t *testing.T) {
	var s stringSlice

	err := s.Set("a")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	err = s.Set("b")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if len(s) != 2 || s[0] != "a" || s[1] != "b" {
		t.Errorf("unexpected slice: %v", s)
	}
}

func TestStringSlice_String(t *testing.T) {
	s := stringSlice{"x", "y"}

	got := s.String()
	if got == "" {
		t.Error("String() should not return empty for non-empty slice")
	}
}

func TestValidateAndSetCustomLabels_Valid(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })

	err := validateAndSetCustomLabels([]string{"team", "tier"})
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
}

func TestValidateAndSetCustomLabels_TooMany(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })
	// 9 labels exceeds the max of 8.
	labels := []string{"a", "b", "c", "d", "e", "f", "g", "h", "i"}

	err := validateAndSetCustomLabels(labels)
	if err == nil {
		t.Error("expected error for too many labels")
	}
}

func TestValidateAndSetCustomLabels_InvalidName_ReservedPrefix(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })

	err := validateAndSetCustomLabels([]string{"__reserved"})
	if err == nil {
		t.Error("expected error for __ prefix label")
	}
}

func TestValidateAndSetCustomLabels_ReservedName(t *testing.T) {
	for _, raw := range []string{"stack", "service", "service_mode", "display.name", "state"} {
		t.Run(raw, func(t *testing.T) {
			t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })

			err := validateAndSetCustomLabels([]string{"team", raw})
			if !errors.Is(err, ErrReservedLabelName) {
				t.Fatalf("err = %v, want ErrReservedLabelName", err)
			}
		})
	}
}

func TestValidateAndSetCustomLabels_PostSanitizeCollision(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })

	err := validateAndSetCustomLabels([]string{"foo.bar", "foo-bar"})
	if err == nil {
		t.Error("expected error for post-sanitize collision")
	}
}

func TestValidateAndSetCustomLabels_Empty(t *testing.T) {
	t.Cleanup(func() { collector.SetCustomLabels(nil, nil) })

	err := validateAndSetCustomLabels(nil)
	if err != nil {
		t.Fatalf("unexpected error for nil input: %v", err)
	}
}

// Not parallel: t.Setenv changes the process environment client.FromEnv reads.
func TestValidateClientAPIVersion(t *testing.T) {
	cases := []struct {
		name       string
		apiVersion string // DOCKER_API_VERSION; "" means unset
		wantErr    bool
	}{
		{name: "unset negotiates from the client maximum", apiVersion: "", wantErr: false},
		{name: "below the minimum", apiVersion: "1.39", wantErr: true},
		{name: "the minimum", apiVersion: "1.40", wantErr: false},
		{name: "the maximum", apiVersion: "1.56", wantErr: false},
		{name: "above the maximum", apiVersion: "1.57", wantErr: true},
		{name: "v prefix", apiVersion: "v1.44", wantErr: false},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			// Keep the rest of the client environment hermetic.
			t.Setenv("DOCKER_HOST", "")
			t.Setenv("DOCKER_TLS_VERIFY", "")
			t.Setenv("DOCKER_CERT_PATH", "")
			t.Setenv("DOCKER_API_VERSION", tc.apiVersion)

			dockerClient, newErr := client.New(client.FromEnv)
			if newErr != nil {
				t.Fatalf("client.New: %v", newErr)
			}

			t.Cleanup(func() { _ = dockerClient.Close() })

			err := validateClientAPIVersion(dockerClient)
			if gotErr := err != nil; gotErr != tc.wantErr {
				t.Fatalf(
					"validateClientAPIVersion(%q) = %v, want error: %v",
					tc.apiVersion,
					err,
					tc.wantErr,
				)
			}

			if tc.wantErr && !errors.Is(err, ErrUnsupportedAPIVersion) {
				t.Errorf("error %v does not wrap ErrUnsupportedAPIVersion", err)
			}
		})
	}
}

// serveUntilDoneBound is how long serveUntilDone gets to return once its server has stopped.
const serveUntilDoneBound = 5 * time.Second

// waitExitCode returns serveUntilDone's exit code from exitCodes, failing the test if it does not
// arrive within serveUntilDoneBound.
func waitExitCode(t *testing.T, exitCodes <-chan int, reason string) int {
	t.Helper()

	select {
	case exitCode := <-exitCodes:
		return exitCode
	case <-time.After(serveUntilDoneBound):
		t.Fatalf("serveUntilDone did not return within %s after %s", serveUntilDoneBound, reason)

		return 0
	}
}

// freeLocalAddress returns a loopback address on a port that was free when probed.
func freeLocalAddress(t *testing.T) string {
	t.Helper()

	probe, listenErr := new(net.ListenConfig).Listen(t.Context(), "tcp", "127.0.0.1:0")
	if listenErr != nil {
		t.Fatalf("probe a free port: %v", listenErr)
	}

	address := probe.Addr().String()

	closeErr := probe.Close()
	if closeErr != nil {
		t.Fatalf("release the probed port: %v", closeErr)
	}

	return address
}

// waitListening polls address until it accepts a TCP connection, failing the test if it does not
// within serveUntilDoneBound.
func waitListening(t *testing.T, address string) {
	t.Helper()

	deadline := time.Now().Add(serveUntilDoneBound)
	dialer := new(net.Dialer)

	for {
		conn, dialErr := dialer.DialContext(t.Context(), "tcp", address)
		if dialErr == nil {
			_ = conn.Close()

			return
		}

		if time.Now().After(deadline) {
			t.Fatalf("%s not listening within %s: %v", address, serveUntilDoneBound, dialErr)
		}

		select {
		case <-t.Context().Done():
			t.Fatalf("wait for %s: %v", address, t.Context().Err())
		case <-time.After(10 * time.Millisecond):
		}
	}
}

func TestServeUntilDone_ListenFailure(t *testing.T) {
	blocker, listenErr := new(net.ListenConfig).Listen(t.Context(), "tcp", "127.0.0.1:0")
	if listenErr != nil {
		t.Fatalf("occupy a port: %v", listenErr)
	}

	t.Cleanup(func() { _ = blocker.Close() })

	rootContext, cancelRoot := context.WithCancel(t.Context())
	t.Cleanup(cancelRoot)

	// Stands in for the event listener and the poller: it only returns once rootContext ends.
	var workerGroup sync.WaitGroup

	workerDone := make(chan struct{})

	workerGroup.Go(func() {
		<-rootContext.Done()
		close(workerDone)
	})

	exitCodes := make(chan int, 1)

	go func() {
		exitCodes <- serveUntilDone(
			rootContext,
			cancelRoot,
			&workerGroup,
			blocker.Addr().String(),
			http.NotFoundHandler(),
		)
	}()

	exitCode := waitExitCode(t, exitCodes, "a bind failure")
	if exitCode != 1 {
		t.Errorf("exit code = %d, want 1", exitCode)
	}

	select {
	case <-workerDone:
	default:
		t.Error("worker still running after serveUntilDone returned")
	}
}

func TestServeUntilDone_CancelledExitsZero(t *testing.T) {
	rootContext, cancelRoot := context.WithCancel(t.Context())
	t.Cleanup(cancelRoot)

	var workerGroup sync.WaitGroup

	workerGroup.Go(func() { <-rootContext.Done() })

	address := freeLocalAddress(t)
	exitCodes := make(chan int, 1)

	go func() {
		exitCodes <- serveUntilDone(
			rootContext,
			cancelRoot,
			&workerGroup,
			address,
			http.NotFoundHandler(),
		)
	}()

	waitListening(t, address)
	cancelRoot()

	exitCode := waitExitCode(t, exitCodes, "cancellation")
	if exitCode != 0 {
		t.Errorf("exit code = %d, want 0", exitCode)
	}
}

// shutdownEarlyWindow is how long serveHTTP is watched for returning early while a request is
// still in flight. Without the detached shutdown context it returns within about a millisecond.
const shutdownEarlyWindow = 200 * time.Millisecond

func TestServeHTTP_ShutdownWaitsForInFlightRequest(t *testing.T) {
	listener, listenErr := new(net.ListenConfig).Listen(t.Context(), "tcp", "127.0.0.1:0")
	if listenErr != nil {
		t.Fatalf("listen: %v", listenErr)
	}

	started := make(chan struct{})
	release := make(chan struct{})

	handler := http.HandlerFunc(func(responseWriter http.ResponseWriter, _ *http.Request) {
		close(started)
		<-release
		responseWriter.WriteHeader(http.StatusOK)
	})

	parentContext, cancelParent := context.WithCancel(t.Context())
	t.Cleanup(cancelParent)

	serveErrs := make(chan error, 1)

	go func() {
		serveErrs <- serveHTTP(parentContext, listener, handler)
	}()

	type response struct {
		status int
		err    error
	}

	responses := make(chan response, 1)

	go func() {
		// Not t.Context(): the request must outlive the parent context canceled below.
		request, requestErr := http.NewRequestWithContext(
			context.WithoutCancel(t.Context()),
			http.MethodGet,
			"http://"+listener.Addr().String()+"/",
			http.NoBody,
		)
		if requestErr != nil {
			responses <- response{status: 0, err: requestErr}

			return
		}

		httpResponse, doErr := http.DefaultClient.Do(request)
		if doErr != nil {
			responses <- response{status: 0, err: doErr}

			return
		}

		_ = httpResponse.Body.Close()
		responses <- response{status: httpResponse.StatusCode, err: nil}
	}()

	select {
	case <-started:
	case <-time.After(serveUntilDoneBound):
		close(release)
		t.Fatalf("request not received within %s", serveUntilDoneBound)
	}

	cancelParent()

	// Negative assertion: serveHTTP must keep waiting for the in-flight request.
	select {
	case serveErr := <-serveErrs:
		close(release)
		t.Fatalf("serveHTTP returned %v while a request was in flight", serveErr)
	case <-time.After(shutdownEarlyWindow):
	}

	close(release)

	select {
	case serveErr := <-serveErrs:
		if serveErr != nil {
			t.Errorf("serveHTTP = %v, want nil", serveErr)
		}
	case <-time.After(serveUntilDoneBound):
		t.Fatalf(
			"serveHTTP did not return within %s of the request completing",
			serveUntilDoneBound,
		)
	}

	select {
	case got := <-responses:
		if got.err != nil {
			t.Fatalf("GET: %v", got.err)
		}

		if got.status != http.StatusOK {
			t.Errorf("status = %d, want %d", got.status, http.StatusOK)
		}
	case <-time.After(serveUntilDoneBound):
		t.Fatalf("no response within %s", serveUntilDoneBound)
	}
}

// healthcheckServer starts a loopback HTTP server whose /healthz answers with status and body,
// and returns its listen address.
func healthcheckServer(t *testing.T, status int, body string) string {
	t.Helper()

	mux := http.NewServeMux()
	mux.HandleFunc(server.HealthzPath, func(responseWriter http.ResponseWriter, _ *http.Request) {
		responseWriter.WriteHeader(status)
		_, _ = io.WriteString(responseWriter, body)
	})

	testServer := httptest.NewServer(mux)
	t.Cleanup(testServer.Close)

	return testServer.Listener.Addr().String()
}

func TestRunHealthcheck_Healthy(t *testing.T) {
	address := healthcheckServer(t, http.StatusOK, "ok\n")

	var out bytes.Buffer
	if exitCode := runHealthcheck(address, &out); exitCode != 0 {
		t.Errorf("exit code = %d, want 0 (output %q)", exitCode, out.String())
	}
}

func TestRunHealthcheck_UnhealthyPrintsReason(t *testing.T) {
	address := healthcheckServer(t, http.StatusServiceUnavailable, "initial resync not completed\n")

	var out bytes.Buffer
	if exitCode := runHealthcheck(address, &out); exitCode != 1 {
		t.Errorf("exit code = %d, want 1", exitCode)
	}

	if !strings.Contains(out.String(), "503") ||
		!strings.Contains(out.String(), "initial resync not completed") {
		t.Errorf("output = %q, want the status and the reason", out.String())
	}
}

func TestRunHealthcheck_NothingListening(t *testing.T) {
	var out bytes.Buffer
	if exitCode := runHealthcheck(freeLocalAddress(t), &out); exitCode != 1 {
		t.Errorf("exit code = %d, want 1", exitCode)
	}

	if out.Len() == 0 {
		t.Error("output is empty, want the connection error")
	}
}

// TestRunHealthcheck_RedirectIsUnhealthy answers /healthz with a redirect to a path that answers
// 200: the probe must take the redirect itself as the answer, and never request its target.
func TestRunHealthcheck_RedirectIsUnhealthy(t *testing.T) {
	var targetRequests atomic.Int64

	mux := http.NewServeMux()
	mux.HandleFunc(
		server.HealthzPath,
		func(responseWriter http.ResponseWriter, request *http.Request) {
			http.Redirect(responseWriter, request, "/ok", http.StatusFound)
		},
	)
	mux.HandleFunc("/ok", func(responseWriter http.ResponseWriter, _ *http.Request) {
		targetRequests.Add(1)
		responseWriter.WriteHeader(http.StatusOK)
	})

	testServer := httptest.NewServer(mux)
	t.Cleanup(testServer.Close)

	var out bytes.Buffer
	if exitCode := runHealthcheck(testServer.Listener.Addr().String(), &out); exitCode != 1 {
		t.Errorf("exit code = %d, want 1", exitCode)
	}

	if !strings.Contains(out.String(), "302") {
		t.Errorf("output = %q, want the 302 status", out.String())
	}

	if got := targetRequests.Load(); got != 0 {
		t.Errorf("redirect target requested %d times, want 0", got)
	}
}

func TestRunHealthcheck_ReasonBodies(t *testing.T) {
	oversized := strings.Repeat("x", 1<<20)

	tests := []struct {
		name string
		body string
		// wantSuffix is how the single output line must end.
		wantSuffix string
		// wantReasonBytes, when set, bounds how many bytes of the body may be printed.
		wantReasonBytes int
	}{
		{name: "empty", body: "", wantSuffix: "503 Service Unavailable"},
		{
			name:       "no trailing newline",
			body:       "resync outstanding",
			wantSuffix: "503 Service Unavailable: resync outstanding",
		},
		{
			name:       "multi-line",
			body:       "last poll too old\nsecond line\n",
			wantSuffix: "503 Service Unavailable: last poll too old",
		},
		{
			name:            "oversized without a newline",
			body:            oversized,
			wantSuffix:      strings.Repeat("x", healthcheckReasonLimit),
			wantReasonBytes: healthcheckReasonLimit,
		},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			address := healthcheckServer(t, http.StatusServiceUnavailable, test.body)

			var out bytes.Buffer
			if exitCode := runHealthcheck(address, &out); exitCode != 1 {
				t.Errorf("exit code = %d, want 1", exitCode)
			}

			line, rest, _ := strings.Cut(out.String(), "\n")
			if rest != "" {
				t.Errorf("output has more than one line: %q", out.String())
			}

			if !strings.HasSuffix(line, test.wantSuffix) {
				t.Errorf("output = %.200q, want it to end with %.200q", line, test.wantSuffix)
			}

			if test.wantReasonBytes > 0 {
				if printed := strings.Count(line, "x"); printed > test.wantReasonBytes {
					t.Errorf(
						"printed %d bytes of the reason, want at most %d",
						printed,
						test.wantReasonBytes,
					)
				}
			}
		})
	}
}

// TestRunHealthcheck_NoProxy checks the probe's client never routes through a proxy. A behavior
// test alone cannot show it for loopback, which Go already exempts from HTTP_PROXY, so the
// transport is inspected too.
func TestRunHealthcheck_NoProxy(t *testing.T) {
	transport, isTransport := newHealthcheckClient().Transport.(*http.Transport)
	if !isTransport {
		t.Fatalf("transport = %T, want *http.Transport", newHealthcheckClient().Transport)
	}

	if transport.Proxy != nil {
		t.Error("transport Proxy is set, want nil")
	}

	t.Setenv("HTTP_PROXY", "http://127.0.0.1:1")

	address := healthcheckServer(t, http.StatusOK, "ok\n")

	var out bytes.Buffer
	if exitCode := runHealthcheck(address, &out); exitCode != 0 {
		t.Errorf("exit code with HTTP_PROXY set = %d, want 0 (output %q)", exitCode, out.String())
	}
}

func TestHealthcheckURL(t *testing.T) {
	tests := []struct {
		listenAddr string
		want       string
	}{
		{listenAddr: "0.0.0.0:8888", want: "http://127.0.0.1:8888/healthz"},
		{listenAddr: ":8888", want: "http://127.0.0.1:8888/healthz"},
		{listenAddr: "[::]:8888", want: "http://[::1]:8888/healthz"},
		{listenAddr: "10.0.0.5:9000", want: "http://10.0.0.5:9000/healthz"},
		{listenAddr: "[fd00::5]:9000", want: "http://[fd00::5]:9000/healthz"},
	}

	for _, test := range tests {
		got, urlErr := healthcheckURL(test.listenAddr)
		if urlErr != nil {
			t.Errorf("healthcheckURL(%q): %v", test.listenAddr, urlErr)

			continue
		}

		if got != test.want {
			t.Errorf("healthcheckURL(%q) = %q, want %q", test.listenAddr, got, test.want)
		}
	}
}

func TestRunHealthcheck_MalformedListenAddr(t *testing.T) {
	var out bytes.Buffer
	if exitCode := runHealthcheck("8888", &out); exitCode != 1 {
		t.Errorf("exit code = %d, want 1", exitCode)
	}

	if !strings.Contains(out.String(), "missing port in address") {
		t.Errorf("output = %q, want the parse error", out.String())
	}
}
