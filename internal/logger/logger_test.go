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

package logger

import (
	"log/slog"
	"testing"
	"time"
)

func TestParseLevel(t *testing.T) {
	cases := []struct {
		input string
		want  slog.Level
	}{
		{"debug", slog.LevelDebug},
		{"DEBUG", slog.LevelDebug},
		{"info", slog.LevelInfo},
		{"INFO", slog.LevelInfo},
		{"warn", slog.LevelWarn},
		{"WARN", slog.LevelWarn},
		{"warning", slog.LevelWarn},
		{"error", slog.LevelError},
		{"fatal", slog.LevelError},
		{"panic", slog.LevelError},
		{"", slog.LevelInfo},
		{"unknown", slog.LevelInfo},
		{"VERBOSE", slog.LevelInfo},
	}
	for _, tc := range cases {
		got := parseLevel(tc.input)
		if got != tc.want {
			t.Errorf("parseLevel(%q) = %v, want %v", tc.input, got, tc.want)
		}
	}
}

func TestTimeStripper_IncludeTime_ReturnsNil(t *testing.T) {
	fn := timeStripper(true)
	if fn != nil {
		t.Error("timeStripper(true) should return nil (no-op)")
	}
}

func TestTimeStripper_ExcludeTime_DropsTimeKey(t *testing.T) {
	fn := timeStripper(false)
	if fn == nil {
		t.Fatal("timeStripper(false) should return a function")
	}

	timeAttr := slog.Time(slog.TimeKey, time.Now())

	result := fn(nil, timeAttr)
	if !result.Equal(slog.Attr{}) {
		t.Errorf("expected time attr to be dropped, got %v", result)
	}
}

func TestTimeStripper_ExcludeTime_PassesOtherAttrs(t *testing.T) {
	fn := timeStripper(false)
	if fn == nil {
		t.Fatal("timeStripper(false) should return a function")
	}

	other := slog.String("service", "myapp")

	result := fn(nil, other)
	if result.Key != "service" || result.Value.String() != "myapp" {
		t.Errorf("non-time attr should pass through, got %v", result)
	}
}
