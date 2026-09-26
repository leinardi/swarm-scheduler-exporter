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
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"sort"

	"github.com/prometheus/common/expfmt"
	"github.com/prometheus/common/model"
)

const (
	metricNodesByState   = "swarm_cluster_nodes_by_state"
	metricDesired        = "swarm_service_desired_replicas"
	metricRunning        = "swarm_service_running_replicas"
	metricAtDesired      = "swarm_service_at_desired"
	metricSchedulable    = "swarm_service_schedulable_replicas"
	metricReplicasState  = "swarm_task_replicas_state"
	metricUpdateState    = "swarm_service_update_state_info"
	metricUpdateStarted  = "swarm_service_update_started_timestamp_seconds"
	metricUpdateComplete = "swarm_service_update_completed_timestamp_seconds"
	metricHealth         = "swarm_exporter_health"
	metricBuildInfo      = "swarm_exporter_build_info"
	metricContainerState = "swarm_container_state"
	metricPollsTotal     = "swarm_exporter_polls_total"

	labelStack   = "stack"
	labelService = "service"
)

// errHTTPStatus marks a response from the exporter with an unexpected status code.
var errHTTPStatus = errors.New("unexpected HTTP status")

// sample is one scraped series: gauge, counter and untyped values only, which is every family the
// tests look at.
type sample struct {
	name   string
	labels map[string]string
	value  float64
}

// scrape is one parsed read of /metrics.
type scrape struct {
	samples []sample
}

// scrapeMetrics reads and parses baseURL/metrics.
func scrapeMetrics(ctx context.Context, baseURL string) (*scrape, error) {
	body, status, err := httpGet(ctx, baseURL+"/metrics")
	if err != nil {
		return nil, err
	}

	if status != http.StatusOK {
		return nil, fmt.Errorf("GET /metrics: %w %d", errHTTPStatus, status)
	}

	parser := expfmt.NewTextParser(model.UTF8Validation)

	families, err := parser.TextToMetricFamilies(bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("parse /metrics: %w", err)
	}

	parsed := &scrape{}

	for name, family := range families {
		for _, metric := range family.GetMetric() {
			labels := make(map[string]string, len(metric.GetLabel()))
			for _, pair := range metric.GetLabel() {
				labels[pair.GetName()] = pair.GetValue()
			}

			var value float64

			switch {
			case metric.GetGauge() != nil:
				value = metric.GetGauge().GetValue()
			case metric.GetCounter() != nil:
				value = metric.GetCounter().GetValue()
			case metric.GetUntyped() != nil:
				value = metric.GetUntyped().GetValue()
			default:
				continue // histograms and summaries: not asserted on
			}

			parsed.samples = append(
				parsed.samples,
				sample{name: name, labels: labels, value: value},
			)
		}
	}

	return parsed, nil
}

func httpGet(ctx context.Context, url string) (body []byte, status int, err error) {
	callCtx, cancel := context.WithTimeout(ctx, httpTimeout)
	defer cancel()

	request, err := http.NewRequestWithContext(callCtx, http.MethodGet, url, http.NoBody)
	if err != nil {
		return nil, 0, fmt.Errorf("build request for %s: %w", url, err)
	}

	response, err := http.DefaultClient.Do(request)
	if err != nil {
		return nil, 0, fmt.Errorf("GET %s: %w", url, err)
	}
	defer response.Body.Close()

	body, err = io.ReadAll(response.Body)
	if err != nil {
		return nil, 0, fmt.Errorf("read %s: %w", url, err)
	}

	return body, response.StatusCode, nil
}

// find returns the samples of the family name whose labels include every pair in match.
func (s *scrape) find(name string, match map[string]string) []sample {
	var found []sample

	for _, smp := range s.samples {
		if smp.name == name && labelsInclude(smp.labels, match) {
			found = append(found, smp)
		}
	}

	return found
}

// value returns the single sample of name matching match, and whether exactly one matched.
func (s *scrape) value(name string, match map[string]string) (float64, bool) {
	found := s.find(name, match)
	if len(found) != 1 {
		return 0, false
	}

	return found[0].value, true
}

// sum adds up every sample of name matching match.
func (s *scrape) sum(name string, match map[string]string) float64 {
	total := 0.0
	for _, smp := range s.find(name, match) {
		total += smp.value
	}

	return total
}

// has reports whether any sample of name matches match.
func (s *scrape) has(name string, match map[string]string) bool {
	return len(s.find(name, match)) > 0
}

// hasService reports whether the family name has any series for the service.
func (s *scrape) hasService(name string, svc serviceKey) bool {
	return s.has(name, svc.labels())
}

func (svc serviceKey) labels() map[string]string {
	return map[string]string{labelStack: svc.stack, labelService: svc.service}
}

// stackLines renders every series of the stack as sorted exposition-style lines, for failure
// output.
func (s *scrape) stackLines(stack string) []string {
	var lines []string

	for _, smp := range s.samples {
		if smp.labels[labelStack] == stack {
			lines = append(lines, formatSample(smp))
		}
	}

	sort.Strings(lines)

	return lines
}

func formatSample(smp sample) string {
	return formatSeries(smp.name, smp.labels) + " " + formatValue(smp.value)
}

func labelsInclude(labels, match map[string]string) bool {
	for key, wanted := range match {
		got, ok := labels[key]
		if !ok || got != wanted {
			return false
		}
	}

	return true
}
