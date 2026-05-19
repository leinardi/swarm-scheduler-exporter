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
	"bytes"
	"context"
	"log/slog"
	"testing"
	"time"
)

// newTestHandler returns a PlainTextHandler writing to buf with no timestamps.
func newTestHandler(buf *bytes.Buffer, level slog.Level) *PlainTextHandler {
	return newPlainTextHandler(buf, level, false)
}

func makeRecord(level slog.Level, msg string, attrs ...slog.Attr) slog.Record {
	r := slog.NewRecord(time.Time{}, level, msg, 0)
	r.AddAttrs(attrs...)

	return r
}

func TestPlainHandler_BasicLine(t *testing.T) {
	var buf bytes.Buffer

	h := newTestHandler(&buf, slog.LevelInfo)

	r := makeRecord(slog.LevelInfo, "hello world")

	err := h.Handle(context.Background(), r)
	if err != nil {
		t.Fatal(err)
	}

	if got, want := buf.String(), "level=INFO hello world\n"; got != want {
		t.Errorf("got %q, want %q", got, want)
	}
}

func TestPlainHandler_WithAttr(t *testing.T) {
	var buf bytes.Buffer

	h := newTestHandler(&buf, slog.LevelInfo)
	r := makeRecord(slog.LevelInfo, "msg", slog.String("k", "v"))
	_ = h.Handle(context.Background(), r)

	got := buf.String()
	if got != "level=INFO msg k=v\n" {
		t.Errorf("got %q", got)
	}
}

func TestPlainHandler_QuotedValue(t *testing.T) {
	var buf bytes.Buffer

	h := newTestHandler(&buf, slog.LevelInfo)
	r := makeRecord(slog.LevelInfo, "msg", slog.String("k", "hello world"))
	_ = h.Handle(context.Background(), r)

	got := buf.String()
	if got != `level=INFO msg k="hello world"`+"\n" {
		t.Errorf("got %q", got)
	}
}

func TestPlainHandler_LevelVariants(t *testing.T) {
	cases := []struct {
		level slog.Level
		want  string
	}{
		{slog.LevelDebug, "DEBUG"},
		{slog.LevelInfo, "INFO"},
		{slog.LevelWarn, "WARN"},
		{slog.LevelError, "ERROR"},
	}
	for _, tc := range cases {
		var buf bytes.Buffer

		h := newTestHandler(&buf, tc.level)
		r := makeRecord(tc.level, "")
		_ = h.Handle(context.Background(), r)

		got := buf.String()
		if got != "level="+tc.want+"\n" {
			t.Errorf("level %v: got %q", tc.level, got)
		}
	}
}

func TestPlainHandler_Enabled(t *testing.T) {
	var buf bytes.Buffer

	h := newTestHandler(&buf, slog.LevelWarn)
	if h.Enabled(context.Background(), slog.LevelInfo) {
		t.Error("INFO should not be enabled when level=WARN")
	}

	if !h.Enabled(context.Background(), slog.LevelWarn) {
		t.Error("WARN should be enabled")
	}

	if !h.Enabled(context.Background(), slog.LevelError) {
		t.Error("ERROR should be enabled")
	}
}

func TestPlainHandler_WithGroup(t *testing.T) {
	var buf bytes.Buffer

	h := newTestHandler(&buf, slog.LevelInfo)
	h2 := h.WithGroup("g1")
	r := makeRecord(slog.LevelInfo, "msg", slog.String("k", "v"))
	_ = h2.Handle(context.Background(), r)

	got := buf.String()
	if got != "level=INFO msg g1.k=v\n" {
		t.Errorf("got %q", got)
	}
}

func TestPlainHandler_WithAttrs(t *testing.T) {
	var buf bytes.Buffer

	h := newTestHandler(&buf, slog.LevelInfo)
	h2 := h.WithAttrs([]slog.Attr{slog.String("prekey", "preval")})
	r := makeRecord(slog.LevelInfo, "msg", slog.String("k", "v"))
	_ = h2.Handle(context.Background(), r)

	got := buf.String()
	if got != "level=INFO msg prekey=preval k=v\n" {
		t.Errorf("got %q", got)
	}
}

func TestPlainHandler_WithAttrs_Empty_ReturnsSelf(t *testing.T) {
	var buf bytes.Buffer

	h := newTestHandler(&buf, slog.LevelInfo)

	h2 := h.WithAttrs(nil)
	if h2 != h {
		t.Error("WithAttrs(nil) should return the same handler")
	}
}

func TestPlainHandler_WithGroup_EmptyName_ReturnsSelf(t *testing.T) {
	var buf bytes.Buffer

	h := newTestHandler(&buf, slog.LevelInfo)

	h2 := h.WithGroup("   ")
	if h2 != h {
		t.Error("WithGroup with blank name should return the same handler")
	}
}

func TestPlainHandler_OriginalUnmutatedByWithAttrs(t *testing.T) {
	var buf1, buf2 bytes.Buffer

	orig := newTestHandler(&buf1, slog.LevelInfo)

	derived, ok := orig.WithAttrs([]slog.Attr{slog.String("extra", "x")}).(*PlainTextHandler)
	if !ok {
		t.Fatal("WithAttrs must return *PlainTextHandler")
	}

	derived.outputWriter = &buf2

	r := makeRecord(slog.LevelInfo, "msg")
	_ = orig.Handle(context.Background(), r)
	_ = derived.Handle(context.Background(), r)

	if buf1.String() != "level=INFO msg\n" {
		t.Errorf("original handler polluted: %q", buf1.String())
	}

	if buf2.String() != "level=INFO msg extra=x\n" {
		t.Errorf("derived handler wrong: %q", buf2.String())
	}
}

func TestPlainHandler_GroupAttr(t *testing.T) {
	var buf bytes.Buffer

	h := newTestHandler(&buf, slog.LevelInfo)
	r := makeRecord(slog.LevelInfo, "msg", slog.Group("g", slog.Int("n", 42)))
	_ = h.Handle(context.Background(), r)

	got := buf.String()
	if got != "level=INFO msg g={g.n=42}\n" {
		t.Errorf("got %q", got)
	}
}

func TestPlainHandler_IncludeTime(t *testing.T) {
	var buf bytes.Buffer

	h := newPlainTextHandler(&buf, slog.LevelInfo, true)
	ts := time.Date(2025, 1, 15, 10, 0, 0, 0, time.UTC)
	r := slog.NewRecord(ts, slog.LevelInfo, "msg", 0)
	_ = h.Handle(context.Background(), r)

	got := buf.String()
	if got == "" || got[:5] != "time=" {
		t.Errorf("expected line to start with 'time=', got %q", got)
	}
}

func TestLevelToUpper(t *testing.T) {
	cases := []struct {
		level slog.Level
		want  string
	}{
		{slog.LevelDebug, "DEBUG"},
		{slog.LevelDebug - 1, "DEBUG"},
		{slog.LevelInfo, "INFO"},
		{slog.LevelWarn, "WARN"},
		{slog.LevelError, "ERROR"},
		{slog.LevelError + 1, "ERROR"},
	}
	for _, tc := range cases {
		got := levelToUpper(tc.level)
		if got != tc.want {
			t.Errorf("levelToUpper(%v) = %q, want %q", tc.level, got, tc.want)
		}
	}
}

func TestQualify_NoGroups(t *testing.T) {
	attr := slog.String("key", "val")

	got := qualify(nil, attr)
	if got.Key != "key" {
		t.Errorf("got key %q, want 'key'", got.Key)
	}
}

func TestQualify_WithGroups(t *testing.T) {
	attr := slog.String("k", "v")

	got := qualify([]string{"g1", "g2"}, attr)
	if got.Key != "g1.g2.k" {
		t.Errorf("got key %q, want 'g1.g2.k'", got.Key)
	}
}
