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

// Package labels provides helper functions to sanitize and validate Prometheus label keys
// and to warn about likely high-cardinality label values.
package labels

import (
	"strconv"
	"strings"
	"sync"
	"unicode/utf8"

	"github.com/prometheus/client_golang/prometheus"

	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
)

// Prometheus label name must match: [a-zA-Z_][a-zA-Z0-9_]*
// We enforce this strictly during sanitization/validation.

// Magic-number guards / limits.
const (
	maxLabelNameLen           = 100 // reasonable upper bound; Prometheus has no hard limit
	maxCustomLabelCount       = 8   // guardrail against unbounded cardinality
	highCardinalitySampleSize = 64  // length of value sample shown in the warning log
)

// ---- High-cardinality warnings (heuristic, logged once per key) ----

var warnOnce sync.Map // map[string]struct{} keyed by label key

// SanitizeLabelNames converts a slice of label names into Prometheus-compatible names
// by replacing any non [A-Za-z0-9_] with '_', and ensuring the first rune is [A-Za-z_]
// by prefixing '_' if necessary.
func SanitizeLabelNames(originalNames []string) []string {
	dst := make([]string, 0, len(originalNames))
	for _, labelName := range originalNames {
		dst = append(dst, sanitizeName(labelName))
	}

	return dst
}

// SanitizeMetricLabels converts the keys of a prometheus.Labels map into
// Prometheus-compatible names (non-matching -> '_'). Values are passed through unchanged.
func SanitizeMetricLabels(originalLabels prometheus.Labels) prometheus.Labels {
	dst := make(prometheus.Labels, len(originalLabels))
	for labelName, value := range originalLabels {
		dst[sanitizeName(labelName)] = value
	}

	return dst
}

// ValidateAndSanitizeLabelNames sanitizes, then validates label names,
// enforcing reserved-prefix rules and deduplication after sanitization.
func ValidateAndSanitizeLabelNames(originalNames []string) ([]string, error) {
	if len(originalNames) == 0 {
		return nil, nil
	}

	sanitizedNames := SanitizeLabelNames(originalNames)
	seenNames := make(map[string]struct{}, len(sanitizedNames))

	for index, sanitizedName := range sanitizedNames {
		origName := originalNames[index] //nolint:gosec // G602: parallel slices — same length by construction

		// Length bound
		if sanitizedName == "" || len(sanitizedName) > maxLabelNameLen {
			return nil, newLabelError(
				"invalid label name length",
				origName,
				sanitizedName,
			)
		}

		// Reserved prefix
		if strings.HasPrefix(sanitizedName, "__") {
			return nil, newLabelError(
				"label name uses reserved prefix '__'",
				origName,
				sanitizedName,
			)
		}

		// Predicate check for full validity
		if !isValidLabelName(sanitizedName) {
			return nil, newLabelError(
				"label name violates Prometheus constraints",
				origName,
				sanitizedName,
			)
		}

		// Collision after sanitization
		if _, exists := seenNames[sanitizedName]; exists {
			return nil, newLabelError(
				"label name collides after sanitization",
				origName,
				sanitizedName,
			)
		}

		seenNames[sanitizedName] = struct{}{}
	}

	return sanitizedNames, nil
}

// ValidateCustomLabelCount enforces a maximum number of custom label keys.
func ValidateCustomLabelCount(count int) error {
	if count > maxCustomLabelCount {
		return newCountError(count, maxCustomLabelCount)
	}

	return nil
}

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

	logger.L().Warn(
		"label appears high-cardinality; consider avoiding IDs/hashes as metric labels",
		"label", labelKey,
		"sample_value", truncate(labelValue, highCardinalitySampleSize),
	)
}

// ---- internal helpers ----

// sanitizeName maps labelName to a Prometheus label name: every rune outside [A-Za-z0-9_] becomes
// '_', non-ASCII letters and invalid bytes included, and '_' is prefixed when the result would not
// start with [A-Za-z_]. The result is ASCII.
func sanitizeName(labelName string) string {
	if labelName == "" {
		return "_"
	}

	var builder strings.Builder
	builder.Grow(len(labelName))

	// One '_' per rune: a multi-byte rune starts with a byte >= 0x80, never a label-name byte.
	for index := 0; index < len(labelName); {
		_, size := utf8.DecodeRuneInString(labelName[index:])

		if isLabelNameByte(labelName[index]) {
			builder.WriteByte(labelName[index])
		} else {
			builder.WriteByte('_')
		}

		index += size
	}

	out := builder.String()
	// The output is ASCII, so its first byte is its first character.
	if !isLabelNameStartByte(out[0]) {
		out = "_" + out
	}

	return out
}

// isValidLabelName reports whether labelName matches [a-zA-Z_][a-zA-Z0-9_]*.
func isValidLabelName(labelName string) bool {
	if labelName == "" || !isLabelNameStartByte(labelName[0]) {
		return false
	}

	for index := 1; index < len(labelName); index++ {
		if !isLabelNameByte(labelName[index]) {
			return false
		}
	}

	return true
}

// isLabelNameStartByte reports whether char may start a label name: [A-Za-z_].
func isLabelNameStartByte(char byte) bool {
	return char == '_' || ('a' <= char && char <= 'z') || ('A' <= char && char <= 'Z')
}

// isLabelNameByte reports whether char may appear in a label name: [A-Za-z0-9_].
func isLabelNameByte(char byte) bool {
	return isLabelNameStartByte(char) || ('0' <= char && char <= '9')
}

// truncate returns at most maxLength bytes of input, cut on a rune boundary so a multi-byte
// character is never split.
func truncate(input string, maxLength int) string {
	if len(input) <= maxLength {
		return input
	}

	cut := maxLength
	for cut > 0 && !utf8.RuneStart(input[cut]) {
		cut--
	}

	return input[:cut]
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

func isHexString(hexString string) bool {
	for _, ch := range hexString {
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
	return "too many custom labels: " + strconv.Itoa(
		e.count,
	) + " (max " + strconv.Itoa(
		e.maximum,
	) + ")"
}

func newCountError(count, maximum int) error {
	return &countError{count: count, maximum: maximum}
}
