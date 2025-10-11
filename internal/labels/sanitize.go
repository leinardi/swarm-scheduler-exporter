package labels

import (
	"strings"

	"github.com/prometheus/client_golang/prometheus"
)

func SanitizeLabelNames(orig []string) []string {
	dst := make([]string, 0, len(orig))

	for _, label := range orig {
		s := strings.ReplaceAll(label, ".", "_")
		dst = append(dst, s)
	}

	return dst
}

func SanitizeMetricLabels(orig prometheus.Labels) prometheus.Labels {
	dst := make(prometheus.Labels)

	for name, val := range orig {
		s := strings.ReplaceAll(name, ".", "_")
		dst[s] = val
	}

	return dst
}
