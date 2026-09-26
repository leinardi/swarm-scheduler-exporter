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
	"errors"
	"fmt"
	"maps"
	"slices"
	"strconv"
	"strings"
	"testing"
	"text/tabwriter"
	"time"
)

const (
	// pollInterval is how often eventually re-checks its condition.
	pollInterval = 500 * time.Millisecond
	// httpTimeout bounds one request to the exporter.
	httpTimeout = 5 * time.Second
)

// errNotYet marks a polled condition that does not hold yet.
var errNotYet = errors.New("not yet")

// eventually re-runs check every pollInterval until it returns nil, and fails the test with
// check's last error when timeout passes or the test or suite context ends first. There is no
// sleep anywhere: a fast machine passes as soon as the condition holds, a slow one only costs
// time. check gets the context to use for its own calls.
func eventually(t *testing.T, timeout time.Duration, check func(ctx context.Context) error) {
	t.Helper()

	ctx := testCtx(t)

	err := pollUntil(ctx, timeout, func() error { return check(ctx) })
	if err != nil {
		t.Fatalf("condition not met: %v", err)
	}
}

// waitForFreshPoll waits until the exporter has completed a poll that started after this call, so
// the next scrape reflects the Swarm state as of now. A poll that was already running when this
// was called can finish first, hence the second one.
func waitForFreshPoll(t *testing.T, baseURL string) {
	t.Helper()

	start := -1.0

	eventually(t, 30*time.Second, func(ctx context.Context) error {
		scraped, err := scrapeMetrics(ctx, baseURL)
		if err != nil {
			return err
		}

		polls, ok := scraped.value(metricPollsTotal, nil)
		if !ok {
			return fmt.Errorf("no %s series: %w", metricPollsTotal, errNotYet)
		}

		if start < 0 {
			start = polls
		}

		if polls < start+2 {
			return fmt.Errorf(
				"%s at %g, waiting for %g: %w",
				metricPollsTotal,
				polls,
				start+2,
				errNotYet,
			)
		}

		return nil
	})
}

// metricWant is one expectation on a scrape.
type metricWant struct {
	name     string
	labels   map[string]string
	value    float64
	sum      bool // compare the sum over every matching series instead of a single series
	absent   bool // expect no matching series at all
	positive bool // expect a single series with any value above zero (value is ignored)
}

func want(name string, labels map[string]string, value float64) metricWant {
	return metricWant{name: name, labels: labels, value: value}
}

func wantSum(name string, labels map[string]string, value float64) metricWant {
	return metricWant{name: name, labels: labels, value: value, sum: true}
}

func wantPositive(name string, labels map[string]string) metricWant {
	return metricWant{name: name, labels: labels, positive: true}
}

func wantAbsent(name string, labels map[string]string) metricWant {
	return metricWant{name: name, labels: labels, absent: true}
}

func wantService(name string, svc serviceKey, value float64) metricWant {
	return metricWant{name: name, labels: svc.labels(), value: value}
}

// check evaluates the expectation against one scrape and returns the actual value as shown in
// failure tables.
func (w *metricWant) check(scraped *scrape) (string, bool) {
	found := scraped.find(w.name, w.labels)

	switch {
	case w.absent:
		if len(found) == 0 {
			return "absent", true
		}

		return fmt.Sprintf("%d series", len(found)), false
	case w.sum:
		// A family that only emits the combinations present (nodes_by_state) drops a series
		// rather than zeroing it, so no series sums to zero.
		total := scraped.sum(w.name, w.labels)

		return formatValue(total), total == w.value
	case len(found) == 0:
		return "absent", false
	case len(found) > 1:
		return fmt.Sprintf("%d series", len(found)), false
	case w.positive:
		return formatValue(found[0].value), found[0].value > 0
	default:
		return formatValue(found[0].value), found[0].value == w.value
	}
}

func (w *metricWant) expected() string {
	switch {
	case w.absent:
		return "absent"
	case w.sum:
		return "sum " + formatValue(w.value)
	case w.positive:
		return "> 0"
	default:
		return formatValue(w.value)
	}
}

// metricsMismatchError is the failure of metricsMatch: an expected-vs-actual table plus the stack's
// series in the same scrape, so a timeout shows what the exporter said, not only that it was
// wrong.
type metricsMismatchError struct {
	table      string
	stack      string
	stackLines []string
}

func (m *metricsMismatchError) Error() string {
	var builder strings.Builder

	builder.WriteString("metrics mismatch:\n")
	builder.WriteString(m.table)

	if m.stack != "" {
		fmt.Fprintf(&builder, "last scrape, stack %q:\n", m.stack)

		for _, line := range m.stackLines {
			builder.WriteString("  " + line + "\n")
		}
	}

	return builder.String()
}

func (*metricsMismatchError) Unwrap() error { return errNotYet }

// metricsMatch returns an eventually check that scrapes baseURL and holds when every expectation
// does. stack selects the series printed on failure.
func metricsMatch(baseURL, stack string, wants ...metricWant) func(context.Context) error {
	return func(ctx context.Context) error {
		scraped, err := scrapeMetrics(ctx, baseURL)
		if err != nil {
			return err
		}

		return compareMetrics(scraped, stack, wants)
	}
}

func compareMetrics(scraped *scrape, stack string, wants []metricWant) error {
	var table strings.Builder

	writer := tabwriter.NewWriter(&table, 0, 0, 2, ' ', 0)
	_, _ = fmt.Fprintln(writer, "  \tseries\texpected\tactual")

	allMatch := true

	for idx := range wants {
		wanted := &wants[idx]
		actual, ok := wanted.check(scraped)

		marker := "  "
		if !ok {
			marker = "✗ "
			allMatch = false
		}

		_, _ = fmt.Fprintf(
			writer,
			"%s\t%s\t%s\t%s\n",
			marker,
			formatSeries(wanted.name, wanted.labels),
			wanted.expected(),
			actual,
		)
	}

	if allMatch {
		return nil
	}

	_ = writer.Flush()

	return &metricsMismatchError{
		table:      table.String(),
		stack:      stack,
		stackLines: scraped.stackLines(stack),
	}
}

func formatSeries(name string, labels map[string]string) string {
	keys := slices.Sorted(maps.Keys(labels))
	pairs := make([]string, 0, len(keys))

	for _, key := range keys {
		pairs = append(pairs, key+"="+strconv.Quote(labels[key]))
	}

	return name + "{" + strings.Join(pairs, ",") + "}"
}

func formatValue(value float64) string {
	return strconv.FormatFloat(value, 'g', -1, 64)
}
