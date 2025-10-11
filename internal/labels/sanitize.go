// Package labels provides helper functions to sanitize and validate Prometheus label keys
// and to warn about likely high-cardinality label values.
package labels

import (
	"strings"
	"sync"
	"unicode"

	"github.com/prometheus/client_golang/prometheus"
	"github.com/sirupsen/logrus"
)

// Prometheus label name must match: [a-zA-Z_][a-zA-Z0-9_]*
// We enforce this strictly during sanitization/validation.

// Magic-number guards / limits.
const (
	maxLabelNameLen           = 100 // reasonable upper bound; Prometheus has no hard limit
	maxCustomLabelCount       = 8   // guardrail against unbounded cardinality
	highCardinalitySampleSize = 64  // length of value sample shown in the warning log
)

// SanitizeLabelNames converts a slice of label names into Prometheus-compatible names
// by replacing any non [A-Za-z0-9_] with '_', and ensuring the first rune is [A-Za-z_]
// by prefixing '_' if necessary.
func SanitizeLabelNames(orig []string) []string {
	dst := make([]string, 0, len(orig))
	for _, name := range orig {
		dst = append(dst, sanitizeName(name))
	}

	return dst
}

// SanitizeMetricLabels converts the keys of a prometheus.Labels map into
// Prometheus-compatible names (non-matching -> '_'). Values are passed through unchanged.
func SanitizeMetricLabels(orig prometheus.Labels) prometheus.Labels {
	dst := make(prometheus.Labels, len(orig))
	for name, val := range orig {
		dst[sanitizeName(name)] = val
	}

	return dst
}

// ValidateAndSanitizeLabelNames sanitizes, then validates label names,
// enforcing reserved-prefix rules and deduplication after sanitization.
func ValidateAndSanitizeLabelNames(orig []string) ([]string, error) {
	if len(orig) == 0 {
		return nil, nil
	}

	sanitized := SanitizeLabelNames(orig)
	seen := make(map[string]struct{}, len(sanitized))

	for idx := range sanitized {
		sanitizedName := sanitized[idx]

		// Length bound
		if sanitizedName == "" || len(sanitizedName) > maxLabelNameLen {
			return nil, newLabelError("invalid label name length", orig[idx], sanitizedName)
		}

		// Reserved prefix
		if strings.HasPrefix(sanitizedName, "__") {
			return nil, newLabelError(
				"label name uses reserved prefix '__'",
				orig[idx],
				sanitizedName,
			)
		}

		// Predicate check for full validity
		if !isValidLabelName(sanitizedName) {
			return nil, newLabelError(
				"label name violates Prometheus constraints",
				orig[idx],
				sanitizedName,
			)
		}

		// Collision after sanitization
		if _, exists := seen[sanitizedName]; exists {
			return nil, newLabelError(
				"label name collides after sanitization",
				orig[idx],
				sanitizedName,
			)
		}

		seen[sanitizedName] = struct{}{}
	}

	return sanitized, nil
}

// ValidateCustomLabelCount enforces a maximum number of custom label keys.
func ValidateCustomLabelCount(count int) error {
	if count > maxCustomLabelCount {
		return newCountError(count, maxCustomLabelCount)
	}

	return nil
}

// ---- High-cardinality warnings (heuristic, logged once per key) ----

var warnOnce sync.Map // map[string]struct{} keyed by label key

// MaybeWarnHighCardinality logs a one-time warning if a label value looks like a UUID,
// long hex, or similar high-cardinality token. This does not block metric emission.
func MaybeWarnHighCardinality(labelKey, labelValue string) {
	if labelValue == "" {
		return
	}

	if !isLikelyHighCardinalityValue(labelValue) {
		return
	}

	if _, loaded := warnOnce.LoadOrStore(labelKey, struct{}{}); loaded {
		return
	}

	logrus.WithFields(logrus.Fields{
		"label":        labelKey,
		"sample_value": truncate(labelValue, highCardinalitySampleSize),
	}).Warn("Label appears high-cardinality; consider avoiding IDs/hashes as metric labels")
}

// ---- internal helpers ----

func sanitizeName(name string) string {
	if name == "" {
		return "_"
	}

	// Replace any non [A-Za-z0-9_] with '_'
	var builder strings.Builder
	builder.Grow(len(name))

	for _, runeVal := range name {
		if runeVal == '_' || unicode.IsLetter(runeVal) || unicode.IsDigit(runeVal) {
			// Always emit; if first char ends up invalid (digit), we prefix '_' below.
			builder.WriteRune(runeVal)
		} else {
			builder.WriteByte('_')
		}
	}

	out := builder.String()
	// Ensure first char is [A-Za-z_]
	first := rune(out[0])
	if first != '_' && !unicode.IsLetter(first) {
		out = "_" + out
	}

	return out
}

func isValidLabelName(name string) bool {
	if name == "" {
		return false
	}

	first := rune(name[0])
	if first != '_' && !unicode.IsLetter(first) {
		return false
	}

	for _, r := range name[1:] {
		if r != '_' && !unicode.IsLetter(r) && !unicode.IsDigit(r) {
			return false
		}
	}

	return true
}

func truncate(input string, maxLen int) string {
	if len(input) <= maxLen {
		return input
	}

	return input[:maxLen]
}

// Heuristic: detects UUID-like and long-hex tokens (common high-cardinality culprits).
func isLikelyHighCardinalityValue(value string) bool {
	valueLower := strings.ToLower(value)

	// UUID v4 style: 8-4-4-4-12 hex with dashes
	if len(valueLower) == 36 &&
		isHexString(valueLower[0:8]) &&
		valueLower[8] == '-' &&
		isHexString(valueLower[9:13]) &&
		valueLower[13] == '-' &&
		isHexString(valueLower[14:18]) &&
		valueLower[18] == '-' &&
		isHexString(valueLower[19:23]) &&
		valueLower[23] == '-' &&
		isHexString(valueLower[24:36]) {
		return true
	}

	// Long raw hex (e.g., hashes): ≥ 16 hex chars and all hex
	if len(valueLower) >= 16 && isHexString(valueLower) {
		return true
	}

	return false
}

func isHexString(s string) bool {
	for _, ch := range s {
		if (ch < '0' || ch > '9') && (ch < 'a' || ch > 'f') {
			return false
		}
	}

	return true
}

// Lightweight error types to provide clear messages without heavy dependencies.

type labelError struct {
	reason    string
	original  string
	sanitized string
}

func (e *labelError) Error() string {
	return "invalid label: " + e.reason + " (original=" + e.original + ", sanitized=" + e.sanitized + ")"
}

func newLabelError(reason, original, sanitized string) error {
	return &labelError{
		reason:    reason,
		original:  original,
		sanitized: sanitized,
	}
}

type countError struct {
	count   int
	maximum int
}

func (e *countError) Error() string {
	return "too many custom labels: " + itoa(e.count) + " (max " + itoa(e.maximum) + ")"
}

func newCountError(count, maximum int) error {
	return &countError{count: count, maximum: maximum}
}

// tiny itoa helper to avoid pulling strconv into this package.
func itoa(num int) string {
	const digits = "0123456789"

	if num == 0 {
		return "0"
	}

	var buf [20]byte // enough for 64-bit ints

	pos := len(buf)
	for num > 0 {
		pos--
		buf[pos] = digits[num%10]
		num /= 10
	}

	return string(buf[pos:])
}
