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

package labels

import (
	"strings"
	"testing"
)

// resetWarnOnce clears the warnOnce sync.Map so high-cardinality tests are independent.
func resetWarnOnce() {
	warnOnce.Range(func(k, _ any) bool {
		warnOnce.Delete(k)

		return true
	})
}

func TestSanitizeName(t *testing.T) {
	cases := []struct {
		input string
		want  string
	}{
		{"", "_"},
		{"foo", "foo"},
		{"foo_bar", "foo_bar"},
		{"foo-bar", "foo_bar"},
		{"foo.bar", "foo_bar"},
		{"foo bar", "foo_bar"},
		{"123foo", "_123foo"},
		{"_leading", "_leading"},
		{"Ünïcödé", "Ünïcödé"},
		{"__reserved", "__reserved"},
	}
	for _, tc := range cases {
		got := sanitizeName(tc.input)
		if got != tc.want {
			t.Errorf("sanitizeName(%q) = %q, want %q", tc.input, got, tc.want)
		}
	}
}

func TestSanitizeLabelNames(t *testing.T) {
	in := []string{"foo", "foo-bar", "123bad", ""}
	got := SanitizeLabelNames(in)

	want := []string{"foo", "foo_bar", "_123bad", "_"}
	if len(got) != len(want) {
		t.Fatalf("len %d != %d", len(got), len(want))
	}

	for i := range want {
		if got[i] != want[i] {
			t.Errorf("[%d] got %q, want %q", i, got[i], want[i])
		}
	}
}

func TestSanitizeMetricLabels_KeysSanitized_ValuesPreserved(t *testing.T) {
	in := map[string]string{
		"foo-bar": "my value",
		"ok_key":  "other",
	}

	got := SanitizeMetricLabels(in)
	if got["foo_bar"] != "my value" {
		t.Errorf("expected sanitized key 'foo_bar' = %q", got["foo_bar"])
	}

	if got["ok_key"] != "other" {
		t.Errorf("expected ok_key = 'other', got %q", got["ok_key"])
	}
}

func TestValidateAndSanitizeLabelNames(t *testing.T) {
	t.Run("empty input returns nil", func(t *testing.T) {
		out, err := ValidateAndSanitizeLabelNames(nil)
		if err != nil || out != nil {
			t.Errorf("expected nil,nil; got %v, %v", out, err)
		}
	})
	t.Run("valid names returned sanitized", func(t *testing.T) {
		out, err := ValidateAndSanitizeLabelNames([]string{"foo", "bar_baz"})
		if err != nil {
			t.Fatalf("unexpected error: %v", err)
		}

		if len(out) != 2 || out[0] != "foo" || out[1] != "bar_baz" {
			t.Errorf("unexpected output: %v", out)
		}
	})
	t.Run("reserved __ prefix errors", func(t *testing.T) {
		_, err := ValidateAndSanitizeLabelNames([]string{"__reserved"})
		if err == nil {
			t.Fatal("expected error for __ prefix")
		}
	})
	t.Run("too long errors", func(t *testing.T) {
		longName := strings.Repeat("a", maxLabelNameLen+1)

		_, err := ValidateAndSanitizeLabelNames([]string{longName})
		if err == nil {
			t.Fatal("expected error for too-long name")
		}
	})
	t.Run("post-sanitize collision errors", func(t *testing.T) {
		_, err := ValidateAndSanitizeLabelNames([]string{"foo.bar", "foo-bar"})
		if err == nil {
			t.Fatal("expected error for post-sanitize collision (both → foo_bar)")
		}
	})
}

func TestValidateCustomLabelCount(t *testing.T) {
	err := ValidateCustomLabelCount(maxCustomLabelCount)
	if err != nil {
		t.Errorf("at max should be OK, got %v", err)
	}

	err = ValidateCustomLabelCount(maxCustomLabelCount + 1)
	if err == nil {
		t.Error("over max should error")
	}
}

func TestIsLikelyHighCardinalityValue(t *testing.T) {
	cases := []struct {
		input string
		want  bool
	}{
		{"550e8400-e29b-41d4-a716-446655440000", true}, // UUID v4 shape
		{"AABBCCDDEEFF00112233445566778899", true},     // 32 hex uppercase
		{"aabbccddeeff001122334455", true},             // 24 hex → ≥16
		{"aabbccddeeff0011", true},                     // 16 hex → true
		{"aabbccddeeff001", false},                     // 15 hex → false (len < 16)
		{"hello", false},                               // not hex, short
		{"not-a-uuid-but-dashed", false},
		{"", false},
	}
	for _, tc := range cases {
		got := isLikelyHighCardinalityValue(tc.input)
		if got != tc.want {
			t.Errorf("isLikelyHighCardinalityValue(%q) = %v, want %v", tc.input, got, tc.want)
		}
	}
}

func TestIsHexString(t *testing.T) {
	cases := []struct {
		input string
		want  bool
	}{
		{"0123456789abcdef", true},
		{"ABCDEF", false}, // uppercase not in the range the code checks
		{"xyz", false},
		{"", true}, // vacuous
	}
	for _, tc := range cases {
		got := isHexString(tc.input)
		if got != tc.want {
			t.Errorf("isHexString(%q) = %v, want %v", tc.input, got, tc.want)
		}
	}
}

func TestTruncate(t *testing.T) {
	if got := truncate("hello", 10); got != "hello" {
		t.Errorf("short string should pass through, got %q", got)
	}

	if got := truncate("hello world", 5); got != "hello" {
		t.Errorf("expected truncation to 5, got %q", got)
	}

	if got := truncate("hello", 5); got != "hello" {
		t.Errorf("exact length should pass through, got %q", got)
	}
}

func TestMaybeWarnHighCardinality_WarnedOnce(t *testing.T) {
	t.Cleanup(resetWarnOnce)

	// First call for a new UUID-shaped value should store the key.
	uuid := "550e8400-e29b-41d4-a716-446655440000"
	MaybeWarnHighCardinality("test_key", uuid)

	// Verify the key was stored in warnOnce.
	_, loaded := warnOnce.Load("test_key")
	if !loaded {
		t.Error("expected warnOnce to record 'test_key'")
	}

	// Second call must NOT re-store (LoadOrStore returns loaded=true → early return).
	MaybeWarnHighCardinality("test_key", uuid)

	_, loaded2 := warnOnce.Load("test_key")
	if !loaded2 {
		t.Error("key should still be present after second call")
	}
}

func TestMaybeWarnHighCardinality_NoWarnForLowCardinality(t *testing.T) {
	t.Cleanup(resetWarnOnce)
	MaybeWarnHighCardinality("low_key", "simple_value")

	_, loaded := warnOnce.Load("low_key")
	if loaded {
		t.Error("low-cardinality value must not trigger warnOnce storage")
	}
}

func TestMaybeWarnHighCardinality_EmptyValueSkipped(t *testing.T) {
	t.Cleanup(resetWarnOnce)
	MaybeWarnHighCardinality("empty_key", "")

	_, loaded := warnOnce.Load("empty_key")
	if loaded {
		t.Error("empty value must not trigger warnOnce storage")
	}
}
