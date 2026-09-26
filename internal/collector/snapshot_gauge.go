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

// snapshot_gauge publishes gauge families that are rebuilt as a whole on every update. A
// GaugeVec rebuilt with Reset and one Set per series is locked per operation, not across the
// rebuild, so a scrape landing in between sees the family empty or half written. Here the new
// set is built off to the side and swapped in with one atomic store, so a scrape sees either
// the previous set or the new one.

import (
	"errors"
	"fmt"
	"strconv"
	"strings"
	"sync/atomic"

	"github.com/prometheus/client_golang/prometheus"
)

// errSnapshotLabelsMismatch reports a label set whose keys are not exactly the label names of
// its family. GaugeVec.With panics on the same input; a builder fails its build instead.
var errSnapshotLabelsMismatch = errors.New("label set does not match the family's label names")

// snapshotCollector exposes one or more gauge families from a single published set of const
// metrics. Families that share a collector are published together, so a scrape never sees
// them taken from different updates.
type snapshotCollector struct {
	descs   []*prometheus.Desc
	current atomic.Pointer[[]prometheus.Metric]
}

// newSnapshotCollector returns a collector for descs with nothing published yet. Until the
// first publish it emits no series, like a GaugeVec with no children.
func newSnapshotCollector(descs ...*prometheus.Desc) *snapshotCollector {
	return &snapshotCollector{descs: descs}
}

// Describe sends the descriptors of every family of the collector.
func (collector *snapshotCollector) Describe(descChannel chan<- *prometheus.Desc) {
	for _, desc := range collector.descs {
		descChannel <- desc
	}
}

// Collect sends the published set. It loads the set once, so a publish during a scrape does
// not change what this scrape sees.
func (collector *snapshotCollector) Collect(metricChannel chan<- prometheus.Metric) {
	published := collector.current.Load()
	if published == nil {
		return
	}

	for _, metric := range *published {
		metricChannel <- metric
	}
}

// publish replaces the published set with metrics in one atomic swap. metrics must be the
// complete set of every family of the collector, as returned by snapshotBuilder.build; the
// caller must not modify it afterwards.
func (collector *snapshotCollector) publish(metrics []prometheus.Metric) {
	collector.current.Store(&metrics)
}

// snapshotSample is one series of a family: its label values, in label-name order, and value.
type snapshotSample struct {
	labelValues []string
	value       float64
}

// snapshotBuilder accumulates the series of one update before it is published.
type snapshotBuilder struct {
	series map[*prometheus.Desc]map[string]snapshotSample
	err    error
}

// newSnapshotBuilder returns an empty builder.
func newSnapshotBuilder() *snapshotBuilder {
	return &snapshotBuilder{series: make(map[*prometheus.Desc]map[string]snapshotSample)}
}

// set records value for the series of desc with labels. labelNames must be the label names desc
// was built with, in the same order; labels must have exactly those keys. Setting the same label
// set twice keeps the last value, as GaugeVec.With(...).Set does: const metrics with duplicate
// label values would fail the whole Gather instead.
func (builder *snapshotBuilder) set(
	desc *prometheus.Desc,
	labelNames []string,
	labels prometheus.Labels,
	value float64,
) {
	if len(labels) != len(labelNames) {
		builder.fail(
			fmt.Errorf(
				"%w: got %d labels, want %v",
				errSnapshotLabelsMismatch,
				len(labels),
				labelNames,
			),
		)

		return
	}

	labelValues := make([]string, len(labelNames))

	for index, labelName := range labelNames {
		labelValue, found := labels[labelName]
		if !found {
			builder.fail(
				fmt.Errorf(
					"%w: missing %q, want %v",
					errSnapshotLabelsMismatch,
					labelName,
					labelNames,
				),
			)

			return
		}

		labelValues[index] = labelValue
	}

	bySeries, found := builder.series[desc]
	if !found {
		bySeries = make(map[string]snapshotSample)
		builder.series[desc] = bySeries
	}

	bySeries[seriesKey(labelValues)] = snapshotSample{labelValues: labelValues, value: value}
}

// fail keeps the first error of the build; later ones add nothing a fix would not also fix.
func (builder *snapshotBuilder) fail(err error) {
	if builder.err == nil {
		builder.err = err
	}
}

// build returns the recorded series as const gauge metrics. Any error fails the whole build and
// returns no metrics, so the caller keeps the previous set published rather than a partial one.
func (builder *snapshotBuilder) build() ([]prometheus.Metric, error) {
	if builder.err != nil {
		return nil, builder.err
	}

	count := 0
	for _, bySeries := range builder.series {
		count += len(bySeries)
	}

	metrics := make([]prometheus.Metric, 0, count)

	for desc, bySeries := range builder.series {
		for _, sample := range bySeries {
			metric, metricErr := prometheus.NewConstMetric(
				desc,
				prometheus.GaugeValue,
				sample.value,
				sample.labelValues...,
			)
			if metricErr != nil {
				return nil, fmt.Errorf("const metric for %s: %w", desc, metricErr)
			}

			metrics = append(metrics, metric)
		}
	}

	return metrics, nil
}

// seriesKey encodes ordered label values as a map key. Each value is prefixed with its length,
// so no two different value lists share a key whatever characters they contain: joining with a
// separator would make ["a|b", "c"] and ["a", "b|c"] collide, and custom label values come from
// users.
func seriesKey(labelValues []string) string {
	var keyBuilder strings.Builder

	for _, labelValue := range labelValues {
		keyBuilder.WriteString(strconv.Itoa(len(labelValue)))
		keyBuilder.WriteByte(':')
		keyBuilder.WriteString(labelValue)
	}

	return keyBuilder.String()
}
