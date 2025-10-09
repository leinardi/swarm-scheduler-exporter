package labels_test

import (
	"testing"

	"github.com/leinardi/swarm-tasks-exporter/internal/labels"
	"github.com/prometheus/client_golang/prometheus"
)

func TestSanitizeLabelNames(t *testing.T) {
	t.Parallel()

	in := []string{"stack", "service", "service.mode", "team.label"}
	got := labels.SanitizeLabelNames(in)
	want := []string{"stack", "service", "service_mode", "team_label"}

	if len(got) != len(want) {
		t.Fatalf("len mismatch: got %d want %d", len(got), len(want))
	}

	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("index %d: got %q want %q", i, got[i], want[i])
		}
	}
}

func TestSanitizeMetricLabels(t *testing.T) {
	t.Parallel()

	in := prometheus.Labels{
		"service.mode": "replicated",
		"team.label":   "ops",
		"stack":        "demo",
	}
	got := labels.SanitizeMetricLabels(in)

	if got["service_mode"] != "replicated" {
		t.Fatalf("expected service_mode=replicated, got %q", got["service_mode"])
	}

	if got["team_label"] != "ops" {
		t.Fatalf("expected team_label=ops, got %q", got["team_label"])
	}

	if got["stack"] != "demo" {
		t.Fatalf("expected stack=demo, got %q", got["stack"])
	}
	// ensure original dotted keys are not present
	if _, ok := got["service.mode"]; ok {
		t.Fatal("unexpected key service.mode found after sanitization")
	}

	if _, ok := got["team.label"]; ok {
		t.Fatal("unexpected key team.label found after sanitization")
	}
}
