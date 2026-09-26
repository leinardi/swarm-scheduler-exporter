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
	"errors"
	"maps"
	"testing"
	"time"

	"github.com/prometheus/client_golang/prometheus"
)

const (
	testSnapshotFamily      = "test_snapshot"
	testSnapshotOtherFamily = "test_snapshot_other"
	testSnapshotWait        = 5 * time.Second
)

var testSnapshotLabelNames = []string{"first", "second"}

func newTestSnapshotDesc(fqName string) *prometheus.Desc {
	return prometheus.NewDesc(fqName, "test", testSnapshotLabelNames, nil)
}

func testSnapshotLabels(first, second string) prometheus.Labels {
	return prometheus.Labels{"first": first, "second": second}
}

// buildTestSnapshot builds series (first label value -> value, second label value fixed) for desc
// and fails the test if the build fails.
func buildTestSnapshot(
	t *testing.T,
	desc *prometheus.Desc,
	series map[string]float64,
) []prometheus.Metric {
	t.Helper()

	builder := newSnapshotBuilder()
	for first, value := range series {
		builder.set(desc, testSnapshotLabelNames, testSnapshotLabels(first, "x"), value)
	}

	metrics, buildErr := builder.build()
	if buildErr != nil {
		t.Fatalf("build: %v", buildErr)
	}

	return metrics
}

// expectedTestSeries is what gatherSeries returns for a snapshot built by buildTestSnapshot.
func expectedTestSeries(fqName string, series map[string]float64) map[string]float64 {
	expected := make(map[string]float64, len(series))
	for first, value := range series {
		expected[seriesID(fqName, testSnapshotLabels(first, "x"))] = value
	}

	return expected
}

func assertGathered(t *testing.T, collector prometheus.Collector, expected map[string]float64) {
	t.Helper()

	gathered := gatherSeries(t, collector)
	if !maps.Equal(gathered, expected) {
		t.Fatalf("gathered %v, want %v", gathered, expected)
	}
}

func TestSnapshotCollector_NothingPublished_EmitsNoFamily(t *testing.T) {
	desc := newTestSnapshotDesc(testSnapshotFamily)
	collector := newSnapshotCollector(desc)

	assertGathered(t, collector, map[string]float64{})

	collector.publish(buildTestSnapshot(t, desc, nil))

	assertGathered(t, collector, map[string]float64{})
}

// TestSnapshotCollector_PublishDuringCollect_ScrapeKeepsItsSnapshot pins the atomicity of one
// scrape: a publish that lands while Collect is sending does not leak into what it sends.
func TestSnapshotCollector_PublishDuringCollect_ScrapeKeepsItsSnapshot(t *testing.T) {
	desc := newTestSnapshotDesc(testSnapshotFamily)
	collector := newSnapshotCollector(desc)

	seriesA := map[string]float64{"a1": 1, "a2": 2, "a3": 3}
	seriesB := map[string]float64{"b1": 10, "b2": 20}

	metricsA := buildTestSnapshot(t, desc, seriesA)
	collector.publish(metricsA)

	inA := make(map[prometheus.Metric]bool, len(metricsA))
	for _, metric := range metricsA {
		inA[metric] = true
	}

	// Unbuffered, so Collect blocks on every send until the test receives it.
	metricChannel := make(chan prometheus.Metric)

	go func() {
		collector.Collect(metricChannel)
		close(metricChannel)
	}()

	var received []prometheus.Metric

	select {
	case first := <-metricChannel:
		received = append(received, first)
	case <-time.After(testSnapshotWait):
		t.Fatal("Collect sent no metric")
	}

	// Collect is now blocked mid-scrape on its second send.
	collector.publish(buildTestSnapshot(t, desc, seriesB))

	for metric := range metricChannel {
		received = append(received, metric)
	}

	if len(received) != len(metricsA) {
		t.Fatalf("Collect sent %d metrics, want the %d of snapshot A", len(received), len(metricsA))
	}

	for _, metric := range received {
		if !inA[metric] {
			t.Fatalf("Collect sent %v, which is not from snapshot A", metric.Desc())
		}
	}

	assertGathered(t, collector, expectedTestSeries(testSnapshotFamily, seriesB))
}

func TestSnapshotCollector_FamiliesPublishedTogether(t *testing.T) {
	desc := newTestSnapshotDesc(testSnapshotFamily)
	otherDesc := newTestSnapshotDesc(testSnapshotOtherFamily)
	collector := newSnapshotCollector(desc, otherDesc)

	builder := newSnapshotBuilder()
	builder.set(desc, testSnapshotLabelNames, testSnapshotLabels("a", "b"), 1)
	builder.set(otherDesc, testSnapshotLabelNames, testSnapshotLabels("a", "b"), 2)

	metrics, buildErr := builder.build()
	if buildErr != nil {
		t.Fatalf("build: %v", buildErr)
	}

	collector.publish(metrics)

	assertGathered(t, collector, map[string]float64{
		seriesID(testSnapshotFamily, testSnapshotLabels("a", "b")):      1,
		seriesID(testSnapshotOtherFamily, testSnapshotLabels("a", "b")): 2,
	})
}

func TestSeriesKey_DistinctValueListsNeverCollide(t *testing.T) {
	pairs := []struct {
		name  string
		left  []string
		right []string
	}{
		{name: "separator inside a value", left: []string{"a|b", "c"}, right: []string{"a", "b|c"}},
		{
			name:  "length prefix inside a value",
			left:  []string{"1:a", ""},
			right: []string{"", "1:a"},
		},
		{name: "empty values", left: []string{"", "ab"}, right: []string{"ab", ""}},
	}

	for _, pair := range pairs {
		t.Run(pair.name, func(t *testing.T) {
			if seriesKey(pair.left) == seriesKey(pair.right) {
				t.Fatalf("seriesKey(%q) == seriesKey(%q)", pair.left, pair.right)
			}
		})
	}
}

func TestSnapshotBuilder_SeparatorInValues_StaysTwoSeries(t *testing.T) {
	desc := newTestSnapshotDesc(testSnapshotFamily)
	collector := newSnapshotCollector(desc)

	builder := newSnapshotBuilder()
	builder.set(desc, testSnapshotLabelNames, testSnapshotLabels("a|b", "c"), 1)
	builder.set(desc, testSnapshotLabelNames, testSnapshotLabels("a", "b|c"), 2)

	metrics, buildErr := builder.build()
	if buildErr != nil {
		t.Fatalf("build: %v", buildErr)
	}

	collector.publish(metrics)

	assertGathered(t, collector, map[string]float64{
		seriesID(testSnapshotFamily, testSnapshotLabels("a|b", "c")): 1,
		seriesID(testSnapshotFamily, testSnapshotLabels("a", "b|c")): 2,
	})
}

// TestSnapshotBuilder_DuplicateLabelSet_LastWriteWins: const metrics with the same label values
// fail the whole Gather, so the builder must collapse them the way GaugeVec.With does.
func TestSnapshotBuilder_DuplicateLabelSet_LastWriteWins(t *testing.T) {
	desc := newTestSnapshotDesc(testSnapshotFamily)
	collector := newSnapshotCollector(desc)

	builder := newSnapshotBuilder()
	builder.set(desc, testSnapshotLabelNames, testSnapshotLabels("a", "b"), 1)
	builder.set(desc, testSnapshotLabelNames, testSnapshotLabels("a", "b"), 2)

	metrics, buildErr := builder.build()
	if buildErr != nil {
		t.Fatalf("build: %v", buildErr)
	}

	if len(metrics) != 1 {
		t.Fatalf("built %d metrics, want 1", len(metrics))
	}

	collector.publish(metrics)

	// gatherSeries fails the test on a Gather error, such as a duplicate series.
	value, found := snapshotValue(t, collector, testSnapshotFamily, testSnapshotLabels("a", "b"))
	if !found || value != 2 {
		t.Fatalf("series value %v (found %v), want 2", value, found)
	}
}

func TestSnapshotBuilder_BuildError_KeepsPreviousSnapshot(t *testing.T) {
	desc := newTestSnapshotDesc(testSnapshotFamily)

	cases := []struct {
		name   string
		labels prometheus.Labels
		isErr  error
	}{
		{name: "invalid UTF-8 value", labels: testSnapshotLabels("\xff", "b")},
		{
			name:   "missing label",
			labels: prometheus.Labels{"first": "a", "other": "b"},
			isErr:  errSnapshotLabelsMismatch,
		},
		{
			name:   "extra label",
			labels: prometheus.Labels{"first": "a", "second": "b", "third": "c"},
			isErr:  errSnapshotLabelsMismatch,
		},
	}

	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			collector := newSnapshotCollector(desc)

			seriesA := map[string]float64{"a1": 1, "a2": 2}
			collector.publish(buildTestSnapshot(t, desc, seriesA))

			builder := newSnapshotBuilder()
			builder.set(desc, testSnapshotLabelNames, testSnapshotLabels("ok", "b"), 5)
			builder.set(desc, testSnapshotLabelNames, testCase.labels, 6)

			metrics, buildErr := builder.build()
			if buildErr == nil {
				t.Fatal("build succeeded, want an error")
			}

			if testCase.isErr != nil && !errors.Is(buildErr, testCase.isErr) {
				t.Fatalf("build error %v, want %v", buildErr, testCase.isErr)
			}

			if metrics != nil {
				t.Fatalf("build returned %d metrics with its error, want none", len(metrics))
			}

			// The update flow publishes only on success, so the previous snapshot stays.
			assertGathered(t, collector, expectedTestSeries(testSnapshotFamily, seriesA))
		})
	}
}
