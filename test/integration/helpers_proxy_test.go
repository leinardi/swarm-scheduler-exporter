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
	"io"
	"log"
	"net/http"
	"net/http/httptest"
	"net/http/httputil"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

var (
	serviceListPath = regexp.MustCompile(`^(/v[0-9.]+)?/services$`)
	eventsPath      = regexp.MustCompile(`^(/v[0-9.]+)?/events$`)
)

// dockerProxy sits between the exporter and the manager's Docker API and injects the faults a
// real daemon cannot be made to produce on demand: a failing service list, and an event stream
// that drops, stays unavailable, and comes back without the history it missed. Everything else
// is forwarded unchanged.
type dockerProxy struct {
	server  *httptest.Server
	forward *httputil.ReverseProxy

	mu                  sync.Mutex
	failServiceList     bool
	serviceListFailures int
	eventsDown          bool
	forgetEventHistory  bool
	eventStreams        map[int]context.CancelFunc
	nextEventStream     int
	eventConnects       int
}

// startDockerProxy starts a proxy to the manager's Docker API, closed when the test ends.
func startDockerProxy(t *testing.T) *dockerProxy {
	t.Helper()

	target, err := url.Parse(strings.Replace(cluster.Manager.DockerHost, "tcp://", "http://", 1))
	if err != nil {
		t.Fatalf("parse manager DOCKER_HOST: %v", err)
	}

	forward := httputil.NewSingleHostReverseProxy(target)
	// Stream every chunk as it arrives: the event stream is one long response.
	forward.FlushInterval = -1
	// A stream the proxy cuts on purpose is not worth a log line.
	forward.ErrorLog = log.New(io.Discard, "", 0)

	proxy := &dockerProxy{forward: forward, eventStreams: make(map[int]context.CancelFunc)}
	proxy.server = httptest.NewServer(proxy)

	t.Cleanup(func() {
		proxy.cutEventStreams()
		proxy.server.CloseClientConnections()
		proxy.server.Close()
	})

	return proxy
}

func (p *dockerProxy) ServeHTTP(writer http.ResponseWriter, request *http.Request) {
	switch {
	case request.Method == http.MethodGet && serviceListPath.MatchString(request.URL.Path):
		if p.takeServiceListFailure() {
			http.Error(
				writer,
				`{"message":"injected service list failure"}`,
				http.StatusInternalServerError,
			)

			return
		}
	case eventsPath.MatchString(request.URL.Path):
		p.serveEvents(writer, request)

		return
	}

	p.forward.ServeHTTP(writer, request)
}

// dockerHost is the DOCKER_HOST that reaches the manager through the proxy.
func (p *dockerProxy) dockerHost() string {
	return "tcp://" + p.server.Listener.Addr().String()
}

func (p *dockerProxy) takeServiceListFailure() bool {
	p.mu.Lock()
	defer p.mu.Unlock()

	if p.failServiceList {
		p.serviceListFailures++
	}

	return p.failServiceList
}

// serveEvents forwards one event stream under a context the proxy can cancel, which drops the
// exporter's connection the way a daemon restart or a network cut does.
func (p *dockerProxy) serveEvents(writer http.ResponseWriter, request *http.Request) {
	p.mu.Lock()

	if p.eventsDown {
		p.mu.Unlock()
		http.Error(
			writer,
			`{"message":"injected event stream outage"}`,
			http.StatusServiceUnavailable,
		)

		return
	}

	if p.forgetEventHistory {
		// The daemon's history as the exporter asks for it no longer holds what happened while
		// the stream was down, as after a daemon restart: only a resync can catch that up.
		query := request.URL.Query()
		query.Set("since", strconv.FormatInt(time.Now().Unix(), 10))
		request.URL.RawQuery = query.Encode()
	}

	streamContext, cancel := context.WithCancel(request.Context())
	streamID := p.nextEventStream
	p.nextEventStream++
	p.eventStreams[streamID] = cancel
	p.eventConnects++
	p.mu.Unlock()

	defer func() {
		p.mu.Lock()
		delete(p.eventStreams, streamID)
		p.mu.Unlock()
		cancel()
	}()

	p.forward.ServeHTTP(writer, request.WithContext(streamContext))
}

// setServiceListFailing makes every service list fail with a 500, or pass again.
func (p *dockerProxy) setServiceListFailing(failing bool) {
	p.mu.Lock()
	defer p.mu.Unlock()

	p.failServiceList = failing
}

func (p *dockerProxy) serviceListFailureCount() int {
	p.mu.Lock()
	defer p.mu.Unlock()

	return p.serviceListFailures
}

func (p *dockerProxy) eventConnectCount() int {
	p.mu.Lock()
	defer p.mu.Unlock()

	return p.eventConnects
}

// cutEventStreams drops every open event stream and answers new ones with a 503 until
// restoreEventStreams.
func (p *dockerProxy) cutEventStreams() {
	p.mu.Lock()
	defer p.mu.Unlock()

	p.eventsDown = true

	for _, cancel := range p.eventStreams {
		cancel()
	}
}

// restoreEventStreams lets event streams connect again, from now on instead of from the since the
// exporter asks for, so nothing that happened during the outage is replayed.
func (p *dockerProxy) restoreEventStreams() {
	p.mu.Lock()
	defer p.mu.Unlock()

	p.eventsDown = false
	p.forgetEventHistory = true
}
