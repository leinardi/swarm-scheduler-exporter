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

// reconciler_poll is the poll protocol between the poller and the reconciler. The poller asks for
// an immutable polling snapshot, lists and counts tasks against it without touching the caches,
// and submits the counts. The reconciler publishes a service's counts only if nothing about the
// service changed in between, so a scrape never pairs task counts with metadata from another
// moment (a job's new iteration, a new replica count, a new mode or placement).

import (
	"context"
	"errors"
	"fmt"
	"maps"
	"slices"
	"time"

	"github.com/moby/moby/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus"

	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
)

// maxCarriedPolls is how many polls in a row a rejected service keeps its previously published
// series. A service rejected more often than that has its series omitted and fails the poll, so
// health turns red instead of the exporter publishing stale numbers indefinitely.
const maxCarriedPolls = 2

// ErrPollPartiallyRejected is returned when a poll was published without some services, whose
// counts were rejected more than maxCarriedPolls polls in a row.
var ErrPollPartiallyRejected = errors.New(
	"poll published without services rejected too many polls in a row",
)

// pollService is one service of a polling snapshot.
type pollService struct {
	generation uint64
	metadata   serviceMetadata
	labels     prometheus.Labels
}

// pollSnapshot is what a poll counts tasks against. It is immutable once built.
type pollSnapshot struct {
	services       map[string]*pollService
	nodes          []swarm.Node
	nodeGeneration uint64
	epoch          uint64
}

// serviceIDs returns the snapshot's service IDs, sorted so iterating them is deterministic.
func (snapshot *pollSnapshot) serviceIDs() []string {
	return slices.Sorted(maps.Keys(snapshot.services))
}

type snapshotReply struct {
	snapshot *pollSnapshot
	err      error
}

// snapshotRequest asks the reconciler for a polling snapshot. reply has room for the answer, so
// the reconciler never blocks on a poller that stopped waiting.
type snapshotRequest struct {
	reply chan snapshotReply
}

// applyRequest submits the counts of a poll made against snapshot.
type applyRequest struct {
	snapshot *pollSnapshot
	counters serviceCounter
	reply    chan error
}

// --- Poller side ---

// requestPollSnapshot asks the reconciler for a polling snapshot, giving up when ctx is done.
func (r *Reconciler) requestPollSnapshot(ctx context.Context) (*pollSnapshot, error) {
	request := snapshotRequest{reply: make(chan snapshotReply, 1)}

	select {
	case r.snapshotRequests <- request:
	case <-ctx.Done():
		return nil, fmt.Errorf("request polling snapshot: %w", ctx.Err())
	}

	select {
	case reply := <-request.reply:
		return reply.snapshot, reply.err
	case <-ctx.Done():
		return nil, fmt.Errorf("wait for polling snapshot: %w", ctx.Err())
	}
}

// submitPollCounts hands the counts of a poll to the reconciler and waits for the verdict: nil
// when every service of snapshot was published or carried over.
func (r *Reconciler) submitPollCounts(
	ctx context.Context,
	snapshot *pollSnapshot,
	counters serviceCounter,
) error {
	request := applyRequest{snapshot: snapshot, counters: counters, reply: make(chan error, 1)}

	select {
	case r.applyRequests <- request:
	case <-ctx.Done():
		return fmt.Errorf("submit poll counts: %w", ctx.Err())
	}

	select {
	case applyErr := <-request.reply:
		return applyErr
	case <-ctx.Done():
		return fmt.Errorf("wait for poll verdict: %w", ctx.Err())
	}
}

// --- Reconciler side ---

// servePendingPollRequests answers every poll request already waiting, without blocking. A cycle
// starts with it, so a poll never waits behind a burst of service keys.
func (r *Reconciler) servePendingPollRequests(ctx context.Context) {
	for {
		select {
		case request := <-r.snapshotRequests:
			r.serveSnapshotRequest(ctx, request)
		case request := <-r.applyRequests:
			r.serveApplyRequest(ctx, request)
		default:
			return
		}
	}
}

// serveSnapshotRequest answers one accepted snapshot request: with a snapshot, or with the
// context error once the reconciler is shutting down.
func (r *Reconciler) serveSnapshotRequest(ctx context.Context, request snapshotRequest) {
	if r.pollRequestHook != nil {
		r.pollRequestHook()
	}

	if ctx.Err() != nil {
		request.reply <- snapshotReply{snapshot: nil, err: fmt.Errorf("reconciler stopping: %w", ctx.Err())}

		return
	}

	request.reply <- snapshotReply{snapshot: r.buildPollSnapshot(), err: nil}
}

// serveApplyRequest answers one accepted apply request: with the verdict of applying it, or with
// the context error once the reconciler is shutting down.
func (r *Reconciler) serveApplyRequest(ctx context.Context, request applyRequest) {
	if r.pollRequestHook != nil {
		r.pollRequestHook()
	}

	if ctx.Err() != nil {
		request.reply <- fmt.Errorf("reconciler stopping: %w", ctx.Err())

		return
	}

	request.reply <- r.applyPollCounts(request.snapshot, request.counters)
}

// buildPollSnapshot copies what a poll needs from the caches, which only this goroutine writes,
// with the generations and the epoch they correspond to.
func (r *Reconciler) buildPollSnapshot() *pollSnapshot {
	cached := getAllServiceMetadata()
	nodes := getCachedNodes()

	r.mu.Lock()
	defer r.mu.Unlock()

	services := make(map[string]*pollService, len(cached))

	for serviceID := range cached {
		metadata := cached[serviceID]
		services[serviceID] = &pollService{
			generation: r.serviceGeneration[serviceID],
			metadata:   metadata,
			labels:     labelsForMetadata(&metadata),
		}
	}

	return &pollSnapshot{
		services:       services,
		nodes:          nodes,
		nodeGeneration: r.nodeGeneration,
		epoch:          r.epoch,
	}
}

// pollVerdict is the state a poll is judged against, read under mu in one go.
type pollVerdict struct {
	generations    map[string]uint64
	dirty          map[string]bool
	nodeGeneration uint64
	nodesDirty     bool
	epoch          uint64
}

func (r *Reconciler) currentPollVerdict(snapshot *pollSnapshot) pollVerdict {
	r.mu.Lock()
	defer r.mu.Unlock()

	verdict := pollVerdict{
		generations:    make(map[string]uint64, len(snapshot.services)),
		dirty:          make(map[string]bool),
		nodeGeneration: r.nodeGeneration,
		nodesDirty:     r.nodesDirty,
		epoch:          r.epoch,
	}

	for serviceID := range snapshot.services {
		verdict.generations[serviceID] = r.serviceGeneration[serviceID]

		if _, queued := r.pendingSet[serviceID]; queued {
			verdict.dirty[serviceID] = true
		}
	}

	return verdict
}

// accepts reports whether the counts of service can be published: nothing about it changed since
// the snapshot, and no change is waiting to be applied.
func (verdict *pollVerdict) accepts(
	snapshot *pollSnapshot,
	serviceID string,
	service *pollService,
) bool {
	if verdict.epoch != snapshot.epoch ||
		verdict.generations[serviceID] != service.generation ||
		verdict.dirty[serviceID] {
		return false
	}

	nodeDependent := service.metadata.serviceMode == serviceModeGlobal ||
		service.metadata.serviceMode == serviceModeGlobalJob

	// A node change waiting to be applied (a node list in backoff) leaves the node generation where
	// the snapshot saw it, but the snapshot's nodes are already known to be out of date.
	return !nodeDependent ||
		(verdict.nodeGeneration == snapshot.nodeGeneration && !verdict.nodesDirty)
}

// applyPollCounts judges each service of a poll and publishes the result. A service no longer
// cached is dropped and marked dirty. An accepted service is published with the snapshot's labels
// and desired count, which are current since its generation did not move. A rejected one, or one
// the poll has no counts for, keeps its previously published series for up to maxCarriedPolls polls in a row, if its label identity
// did not change; past that it is omitted and the poll fails as a partial failure.
func (r *Reconciler) applyPollCounts(snapshot *pollSnapshot, counters serviceCounter) error {
	cached := getAllServiceMetadata()
	verdict := r.currentPollVerdict(snapshot)
	published := make(serviceCounter, len(snapshot.services))
	rejections := make(map[string]int, len(r.pollRejections))

	var omitted []string

	for serviceID, service := range snapshot.services {
		metadata, stillCached := cached[serviceID]
		if !stillCached {
			r.markServiceDirty(serviceID, time.Now())

			continue
		}

		// A service the poll has no counts for is handled like a rejected one, never skipped:
		// skipping it would drop its series while the poll still counted as a success.
		counter, counted := counters[serviceID]
		if counted && verdict.accepts(snapshot, serviceID, service) {
			published[serviceID] = counter

			continue
		}

		IncPollRejections()

		rejected := r.pollRejections[serviceID] + 1
		rejections[serviceID] = rejected

		previous, hadPrevious := r.lastPublished[serviceID]
		identityUnchanged := hadPrevious &&
			maps.Equal(previous.labels, labelsForMetadata(&metadata))

		switch {
		case rejected <= maxCarriedPolls && identityUnchanged:
			published[serviceID] = previous
		case rejected > maxCarriedPolls:
			omitted = append(omitted, serviceID)
		default:
			// Nothing current to show yet (a new service, a new identity): omitted for now.
		}
	}

	publishErr := updateReplicasStateGauge(published)
	if publishErr != nil {
		return publishErr
	}

	r.lastPublished = published
	r.pollRejections = rejections

	if len(omitted) > 0 {
		slices.Sort(omitted)
		logger.L().Warn("poll published without services rejected too many polls in a row",
			"service_ids", omitted, "limit", maxCarriedPolls)

		return fmt.Errorf("%w: %v", ErrPollPartiallyRejected, omitted)
	}

	return nil
}
