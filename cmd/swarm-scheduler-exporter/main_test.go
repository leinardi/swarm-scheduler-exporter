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
	"context"
	"errors"
	"net"
	"net/http"
	"sync"
	"testing"
	"time"

	"github.com/moby/moby/client"

	"github.com/leinardi/swarm-scheduler-exporter/internal/collector"
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
