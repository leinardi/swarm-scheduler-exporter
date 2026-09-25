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

package collector

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/docker/docker/client"
	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
	"github.com/prometheus/common/expfmt"

	labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
)

// This file pins, against a fake Docker Engine, what every SDK call the exporter makes puts on the
// wire and what the metrics computed from the answers look like. It exists so that an SDK upgrade
// can be proven not to change either: the requests are compared in a normalised form, the known
// wire differences are relaxed only through engineWireAllowances, and the computed metrics are
// compared against a golden file.

const (
	// engineWireTestTimeout bounds a whole test, so a reconnect loop fails instead of hanging.
	engineWireTestTimeout = 10 * time.Second
	// engineWireCancelBound is how long ListenSwarmEvents may take to return once canceled.
	engineWireCancelBound = 5 * time.Second
	// engineWirePollInterval is the tick of waitForEngineWire.
	engineWirePollInterval = 10 * time.Millisecond

	// engineWireDaemonAPIVersion is what the fake engine advertises on /_ping: the API version of
	// the Docker 29 engine this exporter is run against. The client negotiates down to its own
	// maximum.
	engineWireDaemonAPIVersion = "1.56"

	engineWireGoldenFile = "testdata/engine_wire/metrics.golden"

	// engineWireCustomLabel is a service label exported through -label, so the pin covers the
	// custom-label path of every per-service family.
	engineWireCustomLabel = "com.example.team"

	// engineWireReconnectMargin is added to the listener's first backoff when the happy path
	// checks that the stream did not reconnect.
	engineWireReconnectMargin = 250 * time.Millisecond

	// The event fixture of the reconnect case carries this timeNano. The fraction (.2) is below
	// half a second on purpose: since goes out with second resolution, so only then does the
	// 500 ms resume offset cross a second boundary and show up on the wire (…299, not …300).
	engineWireReconnectEventTimeNano = 1789902300200000000
)

var updateEngineWireGolden = flag.Bool(
	"update-engine-wire-golden",
	false,
	"rewrite "+engineWireGoldenFile+" from the current output instead of comparing against it",
)

// engineWireFixtureSets are the response fixtures the fake engine serves, one directory per API
// response shape. Every set describes the same cluster state, so every set must produce the same
// golden metrics.
var engineWireFixtureSets = []string{
	"testdata/engine_wire/api-1.51",
}

// wireExpectations are the request properties that are allowed to differ between SDK versions.
// Everything else about a request is compared through its normalised form.
type wireExpectations struct {
	versionPrefix   string // W1: the /vX.YY prefix of every versioned request
	firstRequest    string // W2: the path of the first request of a run (API version negotiation)
	eventsAccept    string // W3: the Accept header of GET /events
	userAgentPrefix string // W4: the prefix of the User-Agent header
}

// engineWireBaseline is what the github.com/docker/docker v28.5.2 client sends.
var engineWireBaseline = wireExpectations{
	versionPrefix:   "/v1.51",
	firstRequest:    "/_ping",
	eventsAccept:    "",
	userAgentPrefix: "Go-http-client/",
}

// wireAllowance relaxes exactly one of wireExpectations, with the evidence that the difference
// does not change what the daemon does.
type wireAllowance struct {
	id       string // W1..W4
	evidence string
	apply    func(expectations *wireExpectations)
}

// engineWireAllowances is the allowance table. It is empty while v28 is the baseline.
var engineWireAllowances = []wireAllowance{}

// engineWireExcludedFamilies are registered families the golden file deliberately does not pin,
// because their values depend on timing or do not come from the SDK.
var engineWireExcludedFamilies = map[string]string{
	"swarm_exporter_poll_duration_seconds":   "timing-dependent",
	"swarm_exporter_polls_total":             "counts poll cycles, not SDK data",
	"swarm_exporter_poll_errors_total":       "counts poll cycles, not SDK data",
	"swarm_exporter_health":                  "derived from wall-clock poll freshness",
	"swarm_exporter_build_info":              "ldflags, not SDK data",
	"swarm_exporter_events_reconnects_total": "asserted by the reconnect case instead",
}

func engineWireExpectations(t *testing.T) wireExpectations {
	t.Helper()

	expectations := engineWireBaseline

	for _, allowance := range engineWireAllowances {
		if allowance.evidence == "" {
			t.Fatalf("allowance %s has no evidence", allowance.id)
		}

		allowance.apply(&expectations)
	}

	return expectations
}

// --- Request recorder ---

type recordedRequest struct {
	method        string
	rawPath       string
	rawQuery      string
	versionPrefix string // "/v1.51", or "" for an unversioned request such as /_ping
	path          string // rawPath without the version prefix
	query         url.Values
	normalized    string
	accept        string
	userAgent     string
}

type requestRecorder struct {
	t *testing.T

	mu    sync.Mutex
	all   []recordedRequest // the whole run; never reset
	phase []recordedRequest // since the last reset
}

var versionPrefixPattern = regexp.MustCompile(`^(/v\d+\.\d+)(/.*)$`)

func (recorder *requestRecorder) record(request *http.Request) recordedRequest {
	entry := recordedRequest{
		method:    request.Method,
		rawPath:   request.URL.Path,
		rawQuery:  request.URL.RawQuery,
		path:      request.URL.Path,
		query:     request.URL.Query(),
		accept:    request.Header.Get("Accept"),
		userAgent: request.Header.Get("User-Agent"),
	}

	if match := versionPrefixPattern.FindStringSubmatch(request.URL.Path); match != nil {
		entry.versionPrefix = match[1]
		entry.path = match[2]
	}

	entry.normalized = normalizeEngineRequest(recorder.t, entry.method, entry.path, entry.query)

	recorder.mu.Lock()
	defer recorder.mu.Unlock()

	recorder.all = append(recorder.all, entry)
	recorder.phase = append(recorder.phase, entry)

	return entry
}

func (recorder *requestRecorder) reset() {
	recorder.mu.Lock()
	defer recorder.mu.Unlock()

	recorder.phase = nil
}

// phaseRequests returns the requests since the last reset, without API version negotiation,
// which W2 allows to move between phases.
func (recorder *requestRecorder) phaseRequests() []recordedRequest {
	recorder.mu.Lock()
	defer recorder.mu.Unlock()

	out := make([]recordedRequest, 0, len(recorder.phase))
	for index := range recorder.phase {
		if recorder.phase[index].path == "/_ping" {
			continue
		}

		out = append(out, recorder.phase[index])
	}

	return out
}

func (recorder *requestRecorder) allRequests() []recordedRequest {
	recorder.mu.Lock()
	defer recorder.mu.Unlock()

	return append([]recordedRequest(nil), recorder.all...)
}

func normalizedRequests(entries []recordedRequest) []string {
	out := make([]string, 0, len(entries))
	for index := range entries {
		out = append(out, entries[index].normalized)
	}

	return out
}

// normalizeEngineRequest renders a request without the parts that legitimately vary: the version
// prefix (W1), the encoding of filters (decoded to a sorted canonical form) and the value of the
// since/until timestamps (asserted separately).
func normalizeEngineRequest(t *testing.T, method, path string, query url.Values) string {
	t.Helper()

	keys := make([]string, 0, len(query))
	for key := range query {
		keys = append(keys, key)
	}

	sort.Strings(keys)

	parts := make([]string, 0, len(keys))

	for _, key := range keys {
		for _, value := range query[key] {
			switch key {
			case "filters":
				value = canonicalFilters(t, value)
			case "since", "until":
				value = "<time>"
			}

			parts = append(parts, key+"="+value)
		}
	}

	if len(parts) == 0 {
		return method + " " + path
	}

	return method + " " + path + "?" + strings.Join(parts, "&")
}

// decodeFilters accepts both filter encodings the Engine API has used: {"key":{"value":true}}
// and the legacy {"key":["value"]}.
func decodeFilters(t *testing.T, raw string) map[string][]string {
	t.Helper()

	var byKey map[string]json.RawMessage

	err := json.Unmarshal([]byte(raw), &byKey)
	if err != nil {
		t.Errorf("filters %q: %v", raw, err)

		return nil
	}

	decoded := make(map[string][]string, len(byKey))

	for key, rawValues := range byKey {
		var set map[string]bool

		setErr := json.Unmarshal(rawValues, &set)
		if setErr == nil {
			for value := range set {
				decoded[key] = append(decoded[key], value)
			}
		} else {
			var list []string

			listErr := json.Unmarshal(rawValues, &list)
			if listErr != nil {
				t.Errorf("filters %q key %q: %v", raw, key, listErr)

				continue
			}

			decoded[key] = list
		}

		sort.Strings(decoded[key])
	}

	return decoded
}

func canonicalFilters(t *testing.T, raw string) string {
	t.Helper()

	decoded := decodeFilters(t, raw)

	keys := make([]string, 0, len(decoded))
	for key := range decoded {
		keys = append(keys, key)
	}

	sort.Strings(keys)

	parts := make([]string, 0, len(keys))
	for _, key := range keys {
		parts = append(parts, key+":["+strings.Join(decoded[key], ",")+"]")
	}

	return "{" + strings.Join(parts, ";") + "}"
}

// --- Fake engine ---

// eventsConnection scripts one GET /events connection: the event fixture it streams, and whether
// it then holds the connection open until the client goes away (true) or ends it (EOF).
type eventsConnection struct {
	fixture string
	hold    bool
}

type fakeEngine struct {
	t        *testing.T
	fixtures string
	recorder *requestRecorder
	stop     chan struct{}

	// bodyOverrides replaces the answer for a path (without version prefix). Read-only once
	// the server runs.
	bodyOverrides map[string][]byte

	mu           sync.Mutex
	eventsScript []eventsConnection
	eventsServed int
}

func (engine *fakeEngine) ServeHTTP(responseWriter http.ResponseWriter, request *http.Request) {
	entry := engine.recorder.record(request)

	if body, ok := engine.bodyOverrides[entry.path]; ok {
		writeEngineJSON(responseWriter, http.StatusOK, body)

		return
	}

	switch {
	case entry.path == "/_ping":
		responseWriter.Header().Set("Api-Version", engineWireDaemonAPIVersion)
		responseWriter.Header().Set("Ostype", "linux")
		responseWriter.Header().Set("Docker-Experimental", "false")
		responseWriter.Header().Set("Swarm", "active/manager")
		responseWriter.Header().Set("Content-Type", "text/plain; charset=utf-8")
		responseWriter.WriteHeader(http.StatusOK)

		if request.Method == http.MethodGet {
			_, _ = responseWriter.Write([]byte("OK"))
		}
	case entry.path == "/events":
		engine.serveEvents(responseWriter, request)
	case entry.path == "/nodes":
		writeEngineJSON(responseWriter, http.StatusOK, engine.fixture("nodes.json"))
	case entry.path == "/services":
		writeEngineJSON(responseWriter, http.StatusOK, engine.fixture("services.json"))
	case strings.HasPrefix(entry.path, "/services/"):
		serviceID := strings.TrimPrefix(entry.path, "/services/")
		engine.serveObject(responseWriter, "service-"+serviceID+".json", "service "+serviceID)
	case entry.path == "/tasks":
		writeEngineJSON(responseWriter, http.StatusOK, engine.fixture("tasks.json"))
	case entry.path == "/containers/json":
		writeEngineJSON(responseWriter, http.StatusOK, engine.fixture("containers.json"))
	case strings.HasPrefix(entry.path, "/containers/") && strings.HasSuffix(entry.path, "/json"):
		containerID := strings.TrimSuffix(strings.TrimPrefix(entry.path, "/containers/"), "/json")
		engine.serveObject(
			responseWriter,
			"container-"+containerID+".json",
			"container "+containerID,
		)
	default:
		engine.t.Errorf("fake engine: unexpected request %s %s", request.Method, request.URL)
		writeEngineJSON(responseWriter, http.StatusNotFound, []byte(`{"message":"page not found"}`))
	}
}

// serveObject answers an inspect: the fixture when there is one, else the daemon's 404.
func (engine *fakeEngine) serveObject(
	responseWriter http.ResponseWriter,
	fixtureName, what string,
) {
	if !engine.fixtureExists(fixtureName) {
		writeEngineJSON(
			responseWriter,
			http.StatusNotFound,
			[]byte(`{"message":"`+what+` not found"}`),
		)

		return
	}

	writeEngineJSON(responseWriter, http.StatusOK, engine.fixture(fixtureName))
}

func (engine *fakeEngine) serveEvents(responseWriter http.ResponseWriter, request *http.Request) {
	engine.mu.Lock()
	index := engine.eventsServed
	engine.eventsServed++
	script := engine.eventsScript
	engine.mu.Unlock()

	if index >= len(script) {
		// Recorded like every request; the test counts /events requests, so a reconnect loop
		// fails it instead of hanging it.
		writeEngineJSON(
			responseWriter,
			http.StatusInternalServerError,
			[]byte(`{"message":"unexpected extra events request"}`),
		)

		return
	}

	connection := script[index]

	responseWriter.Header().Set("Content-Type", "application/json")
	responseWriter.WriteHeader(http.StatusOK)

	if connection.fixture != "" {
		_, _ = responseWriter.Write(engine.fixture(connection.fixture))
	}

	flusher, ok := responseWriter.(http.Flusher)
	if !ok {
		engine.t.Error("fake engine: response writer cannot flush")

		return
	}

	flusher.Flush()

	if !connection.hold {
		return
	}

	select {
	case <-request.Context().Done():
	case <-engine.stop:
	}
}

func (engine *fakeEngine) scriptEvents(connections ...eventsConnection) {
	engine.mu.Lock()
	defer engine.mu.Unlock()

	engine.eventsScript = connections
	engine.eventsServed = 0
}

func (engine *fakeEngine) fixture(name string) []byte {
	engine.t.Helper()

	body, err := os.ReadFile(filepath.Join(engine.fixtures, filepath.Base(name)))
	if err != nil {
		engine.t.Errorf("fixture %s: %v", name, err)

		return nil
	}

	return body
}

func (engine *fakeEngine) fixtureExists(name string) bool {
	_, err := os.Stat(filepath.Join(engine.fixtures, filepath.Base(name)))

	return err == nil
}

func writeEngineJSON(responseWriter http.ResponseWriter, status int, body []byte) {
	responseWriter.Header().Set("Content-Type", "application/json")
	responseWriter.WriteHeader(status)
	_, _ = responseWriter.Write(body)
}

// startFakeEngine serves fixtureDir and returns it with a real Docker client pointed at it. Like
// main, the client negotiates its API version before the first call.
func startFakeEngine(
	t *testing.T,
	ctx context.Context,
	fixtureDir string,
	bodyOverrides map[string][]byte,
) (*fakeEngine, *client.Client) {
	t.Helper()

	engine := &fakeEngine{
		t:             t,
		fixtures:      fixtureDir,
		recorder:      &requestRecorder{t: t},
		stop:          make(chan struct{}),
		bodyOverrides: bodyOverrides,
	}

	server := httptest.NewServer(engine)
	t.Cleanup(server.Close)
	// Registered after server.Close, so it runs first and releases held /events connections.
	t.Cleanup(func() { close(engine.stop) })

	dockerClient, err := client.NewClientWithOpts(
		client.WithHTTPClient(server.Client()),
		client.WithHost("tcp://"+server.Listener.Addr().String()),
	)
	if err != nil {
		t.Fatalf("docker client: %v", err)
	}

	t.Cleanup(func() { _ = dockerClient.Close() })

	dockerClient.NegotiateAPIVersion(ctx)

	return engine, dockerClient
}

// --- Collector state and registration ---

// recordingRegisterer records every collector the production code registers, so the coverage
// guard can discover metric families from registration rather than from gathered output (a vec
// with no children never appears in Gather).
type recordingRegisterer struct {
	*prometheus.Registry

	mu         sync.Mutex
	collectors []prometheus.Collector
}

func (registerer *recordingRegisterer) Register(collector prometheus.Collector) error {
	registerer.mu.Lock()
	registerer.collectors = append(registerer.collectors, collector)
	registerer.mu.Unlock()

	err := registerer.Registry.Register(collector)
	if err != nil {
		return fmt.Errorf("register: %w", err)
	}

	return nil
}

func (registerer *recordingRegisterer) MustRegister(collectors ...prometheus.Collector) {
	for _, collector := range collectors {
		err := registerer.Register(collector)
		if err != nil {
			panic(err)
		}
	}
}

var describedFQNamePattern = regexp.MustCompile(`fqName: "([^"]+)"`)

// describedFamilies returns the family names of every recorded collector, from Describe.
func (registerer *recordingRegisterer) describedFamilies() map[string]bool {
	registerer.mu.Lock()
	collectors := append([]prometheus.Collector(nil), registerer.collectors...)
	registerer.mu.Unlock()

	descriptions := make(chan *prometheus.Desc)

	go func() {
		for _, collector := range collectors {
			collector.Describe(descriptions)
		}

		close(descriptions)
	}()

	families := make(map[string]bool)

	for description := range descriptions {
		if match := describedFQNamePattern.FindStringSubmatch(description.String()); match != nil {
			families[match[1]] = true
		}
	}

	return families
}

// configureCollectorsLikeMain registers every metric the way main does, with container metrics
// enabled, on a recording registerer that stands in for prometheus.DefaultRegisterer. Every
// package global it touches is restored on cleanup. The install helpers in gauge_helpers_test.go
// are not used here: they swap in vecs with test names and trimmed label sets, and the pin is about
// the production names and labels.
func configureCollectorsLikeMain(t *testing.T) *recordingRegisterer {
	t.Helper()

	restoreCollectorGlobals(t)

	registerer := &recordingRegisterer{Registry: prometheus.NewRegistry()}

	previousRegisterer := prometheus.DefaultRegisterer
	prometheus.DefaultRegisterer = registerer

	t.Cleanup(func() { prometheus.DefaultRegisterer = previousRegisterer })

	sanitized, sanitizeErr := labelutil.ValidateAndSanitizeLabelNames(
		[]string{engineWireCustomLabel},
	)
	if sanitizeErr != nil {
		t.Fatalf("sanitize custom label: %v", sanitizeErr)
	}

	SetCustomLabels([]string{engineWireCustomLabel}, sanitized)

	// Same calls, same order as main.run with -containers -containers-include-swarm.
	ConfigureDesiredReplicasGauge()
	ConfigureReplicasStateGauge()
	ConfigureHealthGauges("test", "none", "unknown")
	ConfigureNodesByStateGauge()
	ConfigureExporterOpsMetrics()
	ConfigureServiceUpdateMetrics()
	EnableContainersMetrics(true, true)
	ConfigureContainersStateGauge()

	return registerer
}

// restoreCollectorGlobals snapshots the package's metric globals and restores them on cleanup,
// so the production vecs this test registers do not leak into other tests.
func restoreCollectorGlobals(t *testing.T) {
	t.Helper()

	desired, schedulable := desiredReplicasGauge, schedulableReplicasGauge
	replicasState, running, atDesired := replicasStateGauge, runningReplicasGauge, atDesiredGauge
	health, buildInfo := exporterHealthGauge, buildInfoGauge
	nodesByState := nodesByStateGauge
	pollDuration, polls, pollErrors, reconnects := pollDurationHistogram, pollsTotalCounter, pollErrorsTotalCounter,
		eventsReconnectsTotalCounter
	updateState, updateStarted, updateCompleted := serviceUpdateStateGauge, serviceUpdateStartedTimestamp,
		serviceUpdateCompletedTimestamp
	containersState, enabled, includeSwarm := containersStateGauge, containersEnabled, containersIncludeSwarm

	// ConfigureContainersStateGauge is a no-op once configured.
	containersStateGauge = nil

	t.Cleanup(func() {
		desiredReplicasGauge, schedulableReplicasGauge = desired, schedulable
		replicasStateGauge, runningReplicasGauge, atDesiredGauge = replicasState, running, atDesired
		exporterHealthGauge, buildInfoGauge = health, buildInfo
		nodesByStateGauge = nodesByState
		pollDurationHistogram, pollsTotalCounter, pollErrorsTotalCounter, eventsReconnectsTotalCounter = pollDuration,
			polls, pollErrors, reconnects
		serviceUpdateStateGauge, serviceUpdateStartedTimestamp, serviceUpdateCompletedTimestamp = updateState,
			updateStarted, updateCompleted
		containersStateGauge, containersEnabled, containersIncludeSwarm = containersState, enabled, includeSwarm
	})
}

// --- Assertions ---

// waitForEngineWire polls cond until it holds or ctx ends, and fails naming what never happened.
func waitForEngineWire(t *testing.T, ctx context.Context, what string, cond func() bool) {
	t.Helper()

	ticker := time.NewTicker(engineWirePollInterval)
	defer ticker.Stop()

	for !cond() {
		select {
		case <-ctx.Done():
			t.Fatalf("timed out waiting for %s", what)
		case <-ticker.C:
		}
	}
}

// assertNoSecondEventsConnection watches the current phase for window and fails as soon as a
// second GET /events appears.
func assertNoSecondEventsConnection(t *testing.T, recorder *requestRecorder, window time.Duration) {
	t.Helper()

	deadline := time.NewTimer(window)
	defer deadline.Stop()

	ticker := time.NewTicker(engineWirePollInterval)
	defer ticker.Stop()

	for {
		if connections := len(requestsTo(recorder.phaseRequests(), "/events")); connections > 1 {
			t.Errorf(
				"happy path: the event stream reconnected (%d /events requests, want exactly 1)",
				connections,
			)

			return
		}

		select {
		case <-deadline.C:
			return
		case <-ticker.C:
		}
	}
}

func assertRequestSequence(t *testing.T, phase string, got, want []string) {
	t.Helper()

	if strings.Join(got, "\n") != strings.Join(want, "\n") {
		t.Errorf("%s requests:\n got: %q\nwant: %q", phase, got, want)
	}
}

func assertRequestMultiset(t *testing.T, phase string, got, want []string) {
	t.Helper()

	sortedGot := append([]string(nil), got...)
	sortedWant := append([]string(nil), want...)

	sort.Strings(sortedGot)
	sort.Strings(sortedWant)

	assertRequestSequence(t, phase+" (as a multiset)", sortedGot, sortedWant)
}

// assertWireProperties checks W1, W3 and W4 on every request of the run, and W2 on its order.
func assertWireProperties(t *testing.T, entries []recordedRequest) {
	t.Helper()

	expectations := engineWireExpectations(t)

	if len(entries) == 0 || entries[0].path != expectations.firstRequest {
		t.Errorf(
			"W2: first request of the run is not %s: %v",
			expectations.firstRequest,
			entries[:min(len(entries), 1)],
		)
	}

	pings := 0

	for index := range entries {
		entry := &entries[index]
		if entry.path == "/_ping" {
			pings++

			continue
		}

		if entry.versionPrefix != expectations.versionPrefix {
			t.Errorf(
				"W1: %s %s has version prefix %q, want %q",
				entry.method,
				entry.rawPath,
				entry.versionPrefix,
				expectations.versionPrefix,
			)
		}

		if entry.path == "/events" && entry.accept != expectations.eventsAccept {
			t.Errorf(
				"W3: GET /events Accept = %q, want %q",
				entry.accept,
				expectations.eventsAccept,
			)
		}

		if !strings.HasPrefix(entry.userAgent, expectations.userAgentPrefix) {
			t.Errorf(
				"W4: %s %s User-Agent = %q, want prefix %q",
				entry.method,
				entry.rawPath,
				entry.userAgent,
				expectations.userAgentPrefix,
			)
		}
	}

	if pings != 1 {
		t.Errorf("W2: %d /_ping requests in the run, want exactly 1", pings)
	}
}

// eventsSince parses the since query of an /events request: "<seconds>" or "<seconds>.<nanos>".
func eventsSince(t *testing.T, entry *recordedRequest) time.Time {
	t.Helper()

	raw := entry.query.Get("since")

	secondsText, nanosText, hasNanos := strings.Cut(raw, ".")

	seconds, secondsErr := strconv.ParseInt(secondsText, 10, 64)
	if secondsErr != nil {
		t.Fatalf("events since %q: %v", raw, secondsErr)
	}

	var nanos int64

	if hasNanos {
		parsed, nanosErr := strconv.ParseInt(nanosText, 10, 64)
		if nanosErr != nil {
			t.Fatalf("events since %q: %v", raw, nanosErr)
		}

		nanos = parsed
	}

	return time.Unix(seconds, nanos)
}

func requestsTo(entries []recordedRequest, path string) []recordedRequest {
	var out []recordedRequest

	for index := range entries {
		if entries[index].path == path {
			out = append(out, entries[index])
		}
	}

	return out
}

// runListener starts ListenSwarmEvents and returns a stop that cancels it and asserts it returned
// a context.Canceled wrap within engineWireCancelBound. stop is idempotent and also registered as
// a cleanup, so a failed wait still stops the listener before earlier cleanups restore the package
// globals its workers read.
func runListener(
	t *testing.T,
	ctx context.Context,
	dockerClient DockerAPI,
	since time.Time,
) (stop func()) {
	t.Helper()

	listenerContext, cancel := context.WithCancel(ctx)
	listenerDone := make(chan error, 1)

	go func() { listenerDone <- ListenSwarmEvents(listenerContext, dockerClient, since) }()

	var stopOnce sync.Once

	stop = func() {
		stopOnce.Do(func() {
			cancel()

			select {
			case err := <-listenerDone:
				if !errors.Is(err, context.Canceled) {
					t.Errorf("ListenSwarmEvents returned %v, want a context.Canceled wrap", err)
				}
			case <-time.After(engineWireCancelBound):
				t.Errorf(
					"ListenSwarmEvents did not return within %s of cancellation",
					engineWireCancelBound,
				)
			}
		})
	}

	t.Cleanup(stop)

	return stop
}

func serviceSeriesLabels(t *testing.T, serviceID string) prometheus.Labels {
	t.Helper()

	metadata, ok := getServiceMetadata(serviceID)
	if !ok {
		t.Fatalf("no cached metadata for %s", serviceID)
	}

	return labelsForMetadata(&metadata)
}

// --- Golden metrics ---

func pinnedFamilies(t *testing.T, registered map[string]bool) []string {
	t.Helper()

	var pinned []string

	for family := range registered {
		if !strings.HasPrefix(family, "swarm_") {
			continue
		}

		if _, excluded := engineWireExcludedFamilies[family]; excluded {
			continue
		}

		pinned = append(pinned, family)
	}

	sort.Strings(pinned)

	return pinned
}

func writeEngineWireGolden(t *testing.T, gatherer prometheus.Gatherer, families []string) {
	t.Helper()

	gathered, err := gatherer.Gather()
	if err != nil {
		t.Fatalf("gather: %v", err)
	}

	wanted := make(map[string]bool, len(families))
	for _, family := range families {
		wanted[family] = true
	}

	var golden bytes.Buffer

	for _, family := range gathered {
		if !wanted[family.GetName()] {
			continue
		}

		_, textErr := expfmt.MetricFamilyToText(&golden, family)
		if textErr != nil {
			t.Fatalf("encode %s: %v", family.GetName(), textErr)
		}
	}

	writeErr := os.WriteFile(engineWireGoldenFile, golden.Bytes(), 0o600)
	if writeErr != nil {
		t.Fatalf("write golden: %v", writeErr)
	}
}

// assertCoverage fails when a registered family escapes the pin: every swarm_ family must be
// excluded on purpose or have at least one series in the golden file, and no golden or exclusion
// entry may name a family that is no longer registered.
func assertCoverage(t *testing.T, registered map[string]bool) {
	t.Helper()

	golden, err := os.ReadFile(engineWireGoldenFile)
	if err != nil {
		t.Fatalf("read golden: %v", err)
	}

	seriesByFamily := make(map[string]int)
	goldenFamilies := make(map[string]bool)

	for line := range strings.SplitSeq(string(golden), "\n") {
		if name, found := strings.CutPrefix(line, "# TYPE "); found {
			goldenFamilies[strings.Fields(name)[0]] = true

			continue
		}

		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}

		name, _, _ := strings.Cut(line, "{")
		name, _, _ = strings.Cut(name, " ")
		seriesByFamily[name]++
	}

	for family := range registered {
		if !strings.HasPrefix(family, "swarm_") {
			continue
		}

		if _, excluded := engineWireExcludedFamilies[family]; excluded {
			continue
		}

		if seriesByFamily[family] == 0 {
			t.Errorf(
				"coverage: registered family %s has no series in %s; extend the fixtures",
				family,
				engineWireGoldenFile,
			)
		}
	}

	for family := range goldenFamilies {
		if !registered[family] {
			t.Errorf(
				"coverage: %s pins %s, which is no longer registered",
				engineWireGoldenFile,
				family,
			)
		}
	}

	for family := range engineWireExcludedFamilies {
		if !registered[family] {
			t.Errorf("coverage: exclusion names %s, which is no longer registered", family)
		}
	}
}

// --- Tests ---

func TestEngineWire_PinsRequestsAndMetrics(t *testing.T) {
	for _, fixtureDir := range engineWireFixtureSets {
		t.Run(filepath.Base(fixtureDir), func(t *testing.T) {
			testEngineWire(t, fixtureDir)
		})
	}
}

func testEngineWire(t *testing.T, fixtureDir string) {
	t.Helper()

	resetCollectorState(t)

	ctx, cancel := context.WithTimeout(context.Background(), engineWireTestTimeout)
	defer cancel()

	registerer := configureCollectorsLikeMain(t)
	engine, dockerClient := startFakeEngine(t, ctx, fixtureDir, nil)
	recorder := engine.recorder

	// Phase 1: startup seeding. Sequential: service list, then node list.
	recorder.reset()

	beforeInit := time.Now()

	since, initErr := InitDesiredReplicasGauge(ctx, dockerClient)
	if initErr != nil {
		t.Fatalf("InitDesiredReplicasGauge: %v", initErr)
	}

	afterInit := time.Now()

	assertRequestSequence(
		t,
		"InitDesiredReplicasGauge",
		normalizedRequests(recorder.phaseRequests()),
		[]string{
			"GET /services",
			"GET /nodes",
		},
	)

	// Phase 2: task poll. Sequential: one service-scoped task list, then an inspect for the task
	// whose service is not cached; the daemon's 404 makes the poll skip it.
	recorder.reset()

	polled, pollErr := PollReplicasState(ctx, dockerClient)
	if pollErr != nil {
		t.Fatalf("PollReplicasState: %v", pollErr)
	}

	UpdateReplicasStateGauge(polled)

	assertRequestSequence(
		t,
		"PollReplicasState",
		normalizedRequests(recorder.phaseRequests()),
		[]string{
			"GET /tasks?filters={service:[svc-agent,svc-api,svc-cron,svc-db]}",
			"GET /services/svc-gone?insertDefaults=false",
		},
	)

	// Phase 3: container poll. Sequential: the list, then one inspect per running or exited
	// container, in list order; the paused one is not inspected.
	recorder.reset()

	containerRows, containersErr := PollContainersState(ctx, dockerClient)
	if containersErr != nil {
		t.Fatalf("PollContainersState: %v", containersErr)
	}

	UpdateContainersStateGauge(containerRows)

	assertRequestSequence(
		t,
		"PollContainersState",
		normalizedRequests(recorder.phaseRequests()),
		[]string{
			"GET /containers/json?all=1",
			"GET /containers/c-compose-web/json",
			"GET /containers/c-compose-worker/json",
			"GET /containers/c-plain/json",
			"GET /containers/c-swarm-api/json",
		},
	)

	// Phase 4: events, happy path. One connection streams a service update and a node update and
	// then stays open. Workers handle the two events concurrently, so their requests are compared
	// as a multiset; only "/events first" is an ordering the code guarantees.
	recorder.reset()
	engine.scriptEvents(eventsConnection{fixture: "events-happy.jsonl", hold: true})

	reconnectsBefore := testutil.ToFloat64(eventsReconnectsTotalCounter)
	wantEventRequests := []string{
		"GET /events?filters={type:[node,service]}&since=<time>",
		"GET /services/svc-api?insertDefaults=false",
		"GET /nodes",
		"GET /services/svc-agent?insertDefaults=false",
	}

	stopListener := runListener(t, ctx, dockerClient, since)
	apiLabels := serviceSeriesLabels(t, "svc-api")

	waitForEngineWire(
		t,
		ctx,
		"every request the two events trigger, and web_api desired replicas = 5",
		func() bool {
			return len(recorder.phaseRequests()) >= len(wantEventRequests) &&
				testutil.ToFloat64(desiredReplicasGauge.With(apiLabels)) == 5
		},
	)

	// Negative assertion: nothing signals "the stream will not reconnect", so give a reconnect one
	// full initial backoff (plus margin) to show up, failing as soon as it does.
	assertNoSecondEventsConnection(t, recorder, backoffInitialDelay+engineWireReconnectMargin)

	stopListener()

	eventRequests := recorder.phaseRequests()
	assertRequestMultiset(
		t,
		"ListenSwarmEvents",
		normalizedRequests(eventRequests),
		wantEventRequests,
	)

	if len(eventRequests) == 0 || eventRequests[0].path != "/events" {
		t.Errorf(
			"ListenSwarmEvents: first request is not GET /events: %v",
			normalizedRequests(eventRequests),
		)
	}

	if connections := requestsTo(eventRequests, "/events"); len(connections) == 1 {
		// The anchor is formatted with second resolution before it is sent.
		gotSince := eventsSince(t, &connections[0])
		if gotSince.Before(beforeInit.Truncate(time.Second)) || gotSince.After(afterInit) {
			t.Errorf("events since = %s, want the Init anchor, taken between %s and %s", gotSince,
				beforeInit, afterInit)
		}
	} else {
		t.Errorf("happy path: %d /events requests, want exactly 1", len(connections))
	}

	if got := testutil.ToFloat64(eventsReconnectsTotalCounter); got != reconnectsBefore {
		t.Errorf("happy path: events_reconnects_total = %v, want %v", got, reconnectsBefore)
	}

	// The computed output of every SDK-derived family, after seeding, both polls and the events.
	registered := registerer.describedFamilies()
	families := pinnedFamilies(t, registered)

	if *updateEngineWireGolden {
		writeEngineWireGolden(t, registerer, families)
	}

	golden, openErr := os.Open(engineWireGoldenFile)
	if openErr != nil {
		t.Fatalf("open golden: %v", openErr)
	}

	defer func() { _ = golden.Close() }()

	compareErr := testutil.GatherAndCompare(registerer, golden, families...)
	if compareErr != nil {
		t.Errorf("metrics differ from %s:\n%v", engineWireGoldenFile, compareErr)
	}

	assertCoverage(t, registered)

	// Phase 5: events, reconnect. The first connection streams one event and ends; the listener
	// counts a reconnect and resumes 500 ms before that event on a second connection, which stays
	// open.
	recorder.reset()
	engine.scriptEvents(
		eventsConnection{fixture: "events-reconnect.jsonl", hold: false},
		eventsConnection{fixture: "", hold: true},
	)

	reconnectsBefore = testutil.ToFloat64(eventsReconnectsTotalCounter)
	wantReconnectRequests := []string{
		"GET /events?filters={type:[node,service]}&since=<time>",
		"GET /services/svc-db?insertDefaults=false",
		"GET /events?filters={type:[node,service]}&since=<time>",
	}

	stopListener = runListener(t, ctx, dockerClient, since)

	waitForEngineWire(t, ctx, "the event's inspect and a second /events connection", func() bool {
		return len(recorder.phaseRequests()) >= len(wantReconnectRequests)
	})

	stopListener()

	reconnectRequests := recorder.phaseRequests()
	assertRequestMultiset(
		t,
		"ListenSwarmEvents reconnect",
		normalizedRequests(reconnectRequests),
		wantReconnectRequests,
	)

	if connections := requestsTo(reconnectRequests, "/events"); len(connections) == 2 {
		lastEvent := time.Unix(0, engineWireReconnectEventTimeNano)
		// Resume 500 ms before the last event, sent with second resolution.
		wantSince := lastEvent.Add(-500 * time.Millisecond).Truncate(time.Second)

		if gotSince := eventsSince(t, &connections[1]); !gotSince.Equal(wantSince) {
			t.Errorf(
				"reconnect since = %s (%q), want %s",
				gotSince,
				connections[1].query.Get("since"),
				wantSince,
			)
		}
	} else {
		t.Errorf("reconnect: %d /events requests, want exactly 2", len(connections))
	}

	if got := testutil.ToFloat64(eventsReconnectsTotalCounter); got != reconnectsBefore+1 {
		t.Errorf("reconnect: events_reconnects_total = %v, want %v", got, reconnectsBefore+1)
	}

	assertWireProperties(t, recorder.allRequests())
}

func TestEngineWire_TaskFilterCap(t *testing.T) {
	resetCollectorState(t)

	ctx, cancel := context.WithTimeout(context.Background(), engineWireTestTimeout)
	defer cancel()

	engine, dockerClient := startFakeEngine(
		t,
		ctx,
		engineWireFixtureSets[0],
		map[string][]byte{"/tasks": []byte("[]")},
	)

	for index := range maxServicesInTaskFilter + 1 {
		setServiceMetadata(fmt.Sprintf("svc-%05d", index), &serviceMetadata{
			serviceMode:  serviceModeReplicated,
			customLabels: map[string]string{},
		})
	}

	engine.recorder.reset()

	_, pollErr := PollReplicasState(ctx, dockerClient)
	if pollErr != nil {
		t.Fatalf("PollReplicasState: %v", pollErr)
	}

	requests := engine.recorder.phaseRequests()
	if len(requests) != 1 || requests[0].path != "/tasks" {
		t.Fatalf("requests = %v, want a single GET /tasks", normalizedRequests(requests))
	}

	services := decodeFilters(t, requests[0].query.Get("filters"))["service"]
	if len(services) != maxServicesInTaskFilter {
		t.Errorf(
			"task filter carries %d services, want the cap of %d",
			len(services),
			maxServicesInTaskFilter,
		)
	}

	assertWireProperties(t, engine.recorder.allRequests())
}
