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
	"io"
	"log/slog"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/moby/moby/api/types/events"
	"github.com/moby/moby/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus/testutil"

	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
)

// startListener runs ListenSwarmEvents on dockerClient until the test ends.
func startListener(t *testing.T, dockerClient DockerAPI, reconciler *Reconciler) {
	t.Helper()

	ctx, cancel := context.WithCancel(context.Background())
	listenerDone := make(chan error, 1)

	go func() { listenerDone <- ListenSwarmEvents(ctx, dockerClient, reconciler, time.Now()) }()

	t.Cleanup(func() {
		cancel()
		<-listenerDone
	})
}

func resyncsRequested(reconciler *Reconciler) uint64 {
	requested, _, _ := reconciler.resyncState()

	return requested
}

func globalDesired(t *testing.T, stack, service string) float64 {
	t.Helper()

	return testutil.ToFloat64(
		desiredReplicasGauge.With(serviceLabels(stack, service, serviceModeGlobal)),
	)
}

// ---- Reconnect ----

func TestListenSwarmEvents_ReconnectResumesFromLastEventAndRequestsResync(t *testing.T) {
	dockerClient := newStreamDocker(newReconcilerDocker(nil, nil))
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	before := resyncsRequested(reconciler)

	startListener(t, dockerClient, reconciler)
	waitOpened(t, dockerClient)

	lastSeen := time.Unix(1_700_000_000, 987_654_321)
	dockerClient.messages(0) <- events.Message{
		Type:     events.NodeEventType,
		Action:   events.ActionUpdate,
		Actor:    events.Actor{ID: "n1"},
		TimeNano: lastSeen.UnixNano(),
	}

	_, firstErr := dockerClient.stream(0)
	firstErr <- errClosedBody

	waitOpened(t, dockerClient)

	wantSince := lastSeen.Add(-eventsSinceMargin).UTC().Format(time.RFC3339Nano)
	if got := dockerClient.since(1); got != wantSince {
		t.Errorf(
			"since after the first reconnect = %s, want %s (last seen event minus the margin)",
			got,
			wantSince,
		)
	}

	eventually(t, "a resync requested by the first reconnect", func() bool {
		return resyncsRequested(reconciler) == before+1
	})

	// A connection that saw no event keeps the anchor, and still requests a resync.
	_, secondErr := dockerClient.stream(1)
	secondErr <- errClosedBody

	waitOpened(t, dockerClient)

	if got := dockerClient.since(2); got != wantSince {
		t.Errorf("since after the second reconnect = %s, want %s", got, wantSince)
	}

	eventually(t, "a resync requested by the second reconnect", func() bool {
		return resyncsRequested(reconciler) == before+2
	})
}

// lockedBuffer is a buffer the listener goroutine writes logs to while the test reads them.
type lockedBuffer struct {
	mu     sync.Mutex
	buffer bytes.Buffer
}

func (b *lockedBuffer) Write(chunk []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()

	b.buffer.Write(chunk)

	return len(chunk), nil
}

func (b *lockedBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()

	return b.buffer.String()
}

// captureLogs installs a JSON logger writing to the returned buffer, and restores the previous
// logger when the test ends. Call it before starting any goroutine that logs, so the restore runs
// after that goroutine's cleanup has stopped it.
func captureLogs(t *testing.T) *lockedBuffer {
	t.Helper()

	previous := logger.L()
	logs := &lockedBuffer{}

	logger.Set(slog.New(slog.NewJSONHandler(logs, &slog.HandlerOptions{Level: slog.LevelDebug})))
	t.Cleanup(func() { logger.Set(previous) })

	return logs
}

// reconnectLogRecord is the part of a reconnect log record the tests assert on.
type reconnectLogRecord struct {
	Level string `json:"level"`
	Msg   string `json:"msg"`
	Err   string `json:"err"`
}

// reconnectLogRecords returns the records of logs that announce a reconnect, in order.
func reconnectLogRecords(t *testing.T, logs string) []reconnectLogRecord {
	t.Helper()

	var records []reconnectLogRecord

	for line := range strings.Lines(logs) {
		var record reconnectLogRecord

		decodeErr := json.Unmarshal([]byte(line), &record)
		if decodeErr != nil {
			t.Fatalf("decode log line %q: %v", line, decodeErr)
		}

		if strings.Contains(record.Msg, "reconnect") {
			records = append(records, record)
		}
	}

	return records
}

// TestListenSwarmEvents_ReconnectLogLevel ends one stream cleanly (io.EOF, as a proxy's idle
// timeout does) and the next with another error: the first is logged at info, the second at warn,
// and both still request a resync.
func TestListenSwarmEvents_ReconnectLogLevel(t *testing.T) {
	logs := captureLogs(t)

	dockerClient := newStreamDocker(newReconcilerDocker(nil, nil))
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	before := resyncsRequested(reconciler)

	startListener(t, dockerClient, reconciler)
	waitOpened(t, dockerClient)

	// The listener logs before its backoff, so a reopened stream means the record is written.
	_, firstErr := dockerClient.stream(0)
	firstErr <- io.EOF

	waitOpened(t, dockerClient)

	eventually(t, "a resync requested after the clean close", func() bool {
		return resyncsRequested(reconciler) == before+1
	})

	_, secondErr := dockerClient.stream(1)
	secondErr <- errClosedBody

	waitOpened(t, dockerClient)

	eventually(t, "a resync requested after the failed stream", func() bool {
		return resyncsRequested(reconciler) == before+2
	})

	records := reconnectLogRecords(t, logs.String())
	want := []reconnectLogRecord{
		{
			Level: "INFO",
			Msg:   "event stream closed by the server; reconnecting",
			Err:   "events stream error: EOF",
		},
		{
			Level: "WARN",
			Msg:   "event stream ended; will reconnect",
			Err:   "events stream error: " + errClosedBody.Error(),
		},
	}

	if len(records) != len(want) {
		t.Fatalf("reconnect log records = %+v, want %+v", records, want)
	}

	for index := range want {
		if records[index] != want[index] {
			t.Errorf("reconnect log record %d = %+v, want %+v", index, records[index], want[index])
		}
	}
}

func TestListenSwarmEvents_FirstConnectionRequestsNoResync(t *testing.T) {
	dockerClient := newStreamDocker(newReconcilerDocker(nil, nil))
	reconciler := newTestReconciler(t, dockerClient)
	completeFirstResync(t, reconciler)

	before := resyncsRequested(reconciler)

	startListener(t, dockerClient, reconciler)
	waitOpened(t, dockerClient)

	// Delivered only after followEventStream's post-open step ran: the first connection's resync
	// is the reconciler's own first resync, already done.
	dockerClient.messages(0) <- events.Message{Type: events.NodeEventType, Actor: events.Actor{ID: "n1"}}

	if got := resyncsRequested(reconciler); got != before {
		t.Errorf("resyncs requested = %d after the first connection, want %d", got, before)
	}
}

func TestReconciler_LostRemoveRecoveredOnReconnect(t *testing.T) {
	inner := newReconcilerDocker([]swarm.Service{
		makeReplicatedService("svc1", "stack", "web", 1),
		makeReplicatedService("svc2", "stack", "db", 1),
	}, nil)
	dockerClient := newStreamDocker(inner)
	reconciler := newTestReconciler(t, dockerClient)

	runReconciler(t, reconciler)
	startListener(t, dockerClient, reconciler)
	waitOpened(t, dockerClient)

	eventually(t, "the first resync", func() bool {
		_, cached := getServiceMetadata("svc2")

		return cached
	})

	// svc2 is removed while the stream is down, and its event never arrives.
	inner.setServices(makeReplicatedService("svc1", "stack", "web", 1))

	_, streamErr := dockerClient.stream(0)
	streamErr <- errClosedBody

	eventually(t, "svc2 dropped by the reconnect resync", func() bool {
		_, cached := getServiceMetadata("svc2")

		return !cached
	})

	if got := desiredSeries(t); got != 1 {
		t.Errorf("desired_replicas series = %d, want 1", got)
	}
}

func TestReconciler_LostNodeEventRecoveredOnReconnect(t *testing.T) {
	inner := newReconcilerDocker(
		[]swarm.Service{makeGlobalService("glb1", "infra", "agent")},
		[]swarm.Node{makeSchedulableNode("n1", "h1")},
	)
	dockerClient := newStreamDocker(inner)
	reconciler := newTestReconciler(t, dockerClient)

	runReconciler(t, reconciler)
	startListener(t, dockerClient, reconciler)
	waitOpened(t, dockerClient)

	eventually(
		t,
		"the first resync",
		func() bool { return globalDesired(t, "infra", "agent") == 1 },
	)

	// A node joins while the stream is down, and its event never arrives.
	inner.setNodes(makeSchedulableNode("n1", "h1"), makeSchedulableNode("n2", "h2"))

	_, streamErr := dockerClient.stream(0)
	streamErr <- errClosedBody

	eventually(t, "the global service desired on both nodes", func() bool {
		return globalDesired(t, "infra", "agent") == 2
	})
}

// ---- Changes during a resync ----

func TestReconciler_ChangesDuringResync(t *testing.T) {
	web := makeReplicatedService("svc1", "stack", "web", 2)

	testCases := []struct {
		name string
		// change runs after ServiceList has read the services, before the resync applies them.
		change func(dockerClient *reconcilerDocker, reconciler *Reconciler)
		// settled reports whether the caches reflect the change.
		settled func() bool
	}{
		{
			name: "create",
			change: func(dockerClient *reconcilerDocker, reconciler *Reconciler) {
				dockerClient.setServices(web, makeReplicatedService("svc2", "stack", "late", 4))
				reconciler.enqueueEvent(serviceEvent("svc2", events.ActionCreate))
			},
			settled: func() bool {
				metadata, cached := getServiceMetadata("svc2")

				return cached && metadata.desiredReplicas == 4
			},
		},
		{
			name: "update",
			change: func(dockerClient *reconcilerDocker, reconciler *Reconciler) {
				dockerClient.setServices(makeReplicatedService("svc1", "stack", "web", 5))
				reconciler.enqueueEvent(serviceEvent("svc1", events.ActionUpdate))
			},
			settled: func() bool {
				metadata, cached := getServiceMetadata("svc1")

				return cached && metadata.desiredReplicas == 5
			},
		},
		{
			name: "remove",
			change: func(dockerClient *reconcilerDocker, reconciler *Reconciler) {
				dockerClient.setServices()
				reconciler.enqueueEvent(serviceEvent("svc1", events.ActionRemove))
			},
			settled: func() bool {
				_, cached := getServiceMetadata("svc1")

				return !cached
			},
		},
	}

	for _, testCase := range testCases {
		t.Run(testCase.name, func(t *testing.T) {
			dockerClient := newReconcilerDocker([]swarm.Service{web}, nil)
			reconciler := newTestReconciler(t, dockerClient)
			completeFirstResync(t, reconciler)

			// The resync lists the services as they were; the change and its event land before
			// it applies them, and the key it marks dirty must survive the resync.
			changed := make(chan struct{})
			dockerClient.afterServiceList = func() {
				dockerClient.afterServiceList = nil
				testCase.change(dockerClient, reconciler)
				close(changed)
			}

			reconciler.requestResync(time.Now(), "test")
			runReconciler(t, reconciler)

			select {
			case <-changed:
			case <-time.After(5 * time.Second):
				t.Fatal("the resync never listed the services")
			}

			eventually(t, "the caches to reflect the "+testCase.name, testCase.settled)

			_, completed, _ := reconciler.resyncState()
			if completed < 2 {
				t.Errorf("resyncs completed = %d, want the requested one done", completed)
			}
		})
	}
}
