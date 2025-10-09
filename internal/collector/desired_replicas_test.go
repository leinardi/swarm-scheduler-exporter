//nolint:testpackage // needs access to package internals (desiredReplicasGauge & serviceMetadata)
package collector

import (
	"testing"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/prometheus/client_golang/prometheus/testutil"
)

func TestSetDesiredReplicasGauge_LabelsAndValue(t *testing.T) {
	t.Parallel()

	// Ensure a clean vector for this test.
	// Configure with one custom label containing a dot to verify sanitization.
	SetCustomLabels([]string{"team.label"})
	ConfigureDesiredReplicasGauge()
	t.Cleanup(func() {
		// Unregister to avoid polluting other tests
		prometheus.Unregister(desiredReplicasGauge)
	})

	metadata := serviceMetadata{
		stack:          "demo",
		service:        "api",
		serviceVersion: "1",
		serviceMode:    "replicated",
		customLabels:   map[string]string{"team.label": "platform"},
	}

	setDesiredReplicasGauge(metadata, 3)

	// Query the vector using the SANITIZED label keys.
	gauge := desiredReplicasGauge.With(prometheus.Labels{
		"stack":        "demo",
		"service":      "api",
		"service_mode": "replicated",
		"team_label":   "platform",
	})

	if got := testutil.ToFloat64(gauge); got != 3 {
		t.Fatalf("gauge value: got %v want %v", got, 3)
	}
}
