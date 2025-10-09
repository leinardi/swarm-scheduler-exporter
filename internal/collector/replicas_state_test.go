//nolint:testpackage // we need access to package internals (cache & getServiceLabels)
package collector

import (
	"context"
	"testing"

	"github.com/docker/docker/api/types/swarm"
	"github.com/prometheus/client_golang/prometheus"
)

// getServiceLabels hits Docker only on cache miss; here we prefill the cache to test pure labeling.
func TestGetServiceLabels_UsesCache(t *testing.T) {
	t.Parallel()

	// Ensure custom labels include something dotted so we verify sanitization
	SetCustomLabels([]string{"team.label"})

	// Seed cache for service "svc1"
	metadataCache["svc1"] = serviceMetadata{
		stack:          "demo",
		service:        "worker",
		serviceVersion: "1",
		serviceMode:    "global",
		customLabels:   map[string]string{"team.label": "infra"},
	}

	var task swarm.Task

	task.ServiceID = "svc1"

	lbls, err := getServiceLabels(context.Background(), nil, &task)
	if err != nil {
		t.Fatalf("getServiceLabels error: %v", err)
	}

	// getServiceLabels returns raw label keys (not sanitized yet).
	want := prometheus.Labels{
		"stack":        "demo",
		"service":      "worker",
		"service_mode": "global",
		"team.label":   "infra",
	}
	if len(lbls) != len(want) {
		t.Fatalf("len mismatch: got %d want %d", len(lbls), len(want))
	}

	for k, v := range want {
		if lbls[k] != v {
			t.Fatalf("label %q: got %q want %q", k, lbls[k], v)
		}
	}
}
