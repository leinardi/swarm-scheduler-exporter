// Package labels provides helper functions to sanitize Prometheus label keys.
// The exporter accepts user-defined label names (from Swarm service annotations)
// which may contain characters (e.g., '.') not allowed in Prometheus label names.
// These helpers convert those keys to valid names by replacing '.' with '_'.
package labels

import (
	"strings"

	"github.com/prometheus/client_golang/prometheus"
)

// SanitizeLabelNames converts a slice of label names into Prometheus-compatible names
// by replacing '.' with '_'. This is used when defining a metric's label set.
func SanitizeLabelNames(orig []string) []string {
	dst := make([]string, 0, len(orig))

	for _, label := range orig {
		s := strings.ReplaceAll(label, ".", "_")
		dst = append(dst, s)
	}

	return dst
}

// SanitizeMetricLabels converts the keys of a prometheus.Labels map into
// Prometheus-compatible names ('.' -> '_'). Values are passed through unchanged.
// This is used when setting metric values at runtime.
func SanitizeMetricLabels(orig prometheus.Labels) prometheus.Labels {
	dst := make(prometheus.Labels)

	for name, val := range orig {
		s := strings.ReplaceAll(name, ".", "_")
		dst[s] = val
	}

	return dst
}
