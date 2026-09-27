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
	"slices"
	"strconv"
	"strings"
	"testing"
	"time"
	"unicode/utf8"
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

// ---- Escaping ----

// plainField is one field a plain line carries. A group is a field with group set and no value;
// its children follow it, with their qualified keys.
type plainField struct {
	key   string
	value string
	group bool
}

// parsePlainFields reads the fields of a plain line after the level and the message: bare
// key=value, quoted keys and values (decoded with strconv.Unquote), and key={...} groups.
func parsePlainFields(t *testing.T, fields string) []plainField {
	t.Helper()

	parsed, rest := parseFieldList(t, fields, false)
	if rest != "" {
		t.Fatalf("trailing text after the fields: %q", rest)
	}

	return parsed
}

// parseFieldList reads space-separated fields until the end of input, or until "}" when
// inBraces; it returns them and the unread input.
func parseFieldList(t *testing.T, input string, inBraces bool) (fields []plainField, rest string) {
	t.Helper()

	for index := 0; input != ""; index++ {
		if inBraces && input[0] == '}' {
			return fields, input
		}

		if index > 0 || !inBraces {
			if input[0] != ' ' {
				t.Fatalf("want a space before a field, have %q", input)
			}

			input = input[1:]
		}

		var key string

		key, input = parseToken(t, input, "=")
		if input == "" || input[0] != '=' {
			t.Fatalf("key %q has no '='", key)
		}

		input = input[1:]

		if input != "" && input[0] == '{' {
			fields = append(fields, plainField{key: key, group: true})

			var children []plainField

			children, input = parseFieldList(t, input[1:], true)
			if input == "" || input[0] != '}' {
				t.Fatalf("group %q is not closed", key)
			}

			input = input[1:]

			fields = append(fields, children...)

			continue
		}

		var value string

		value, input = parseToken(t, input, " }")
		fields = append(fields, plainField{key: key, value: value})
	}

	return fields, input
}

// parseToken reads a quoted string, decoded, or a bare token ending before any byte of stop.
func parseToken(t *testing.T, input, stop string) (token, rest string) {
	t.Helper()

	if input != "" && input[0] == '"' {
		quoted, err := strconv.QuotedPrefix(input)
		if err != nil {
			t.Fatalf("bad quoted token in %q: %v", input, err)
		}

		unquoted, err := strconv.Unquote(quoted)
		if err != nil {
			t.Fatalf("unquote %q: %v", quoted, err)
		}

		return unquoted, input[len(quoted):]
	}

	end := strings.IndexAny(input, stop)
	if end < 0 {
		end = len(input)
	}

	return input[:end], input[end:]
}

// plainPosition is where an escaping case puts its text.
type plainPosition int

const (
	positionMessage plainPosition = iota
	positionTopKey
	positionGroupName
	positionGroupChildKey
	positionStringValue
	positionErrorValue
	positionGroupValue
)

var plainPositionNames = map[plainPosition]string{
	positionMessage:       "message",
	positionTopKey:        "top-level key",
	positionGroupName:     "WithGroup name",
	positionGroupChildKey: "grouped child key",
	positionStringValue:   "string value",
	positionErrorValue:    "error value",
	positionGroupValue:    "value inside a group",
}

var (
	textPositions = []plainPosition{
		positionMessage, positionTopKey, positionGroupChildKey,
		positionStringValue, positionErrorValue, positionGroupValue,
	}
	keyPositions   = []plainPosition{positionTopKey, positionGroupName, positionGroupChildKey}
	valuePositions = []plainPosition{positionStringValue, positionErrorValue, positionGroupValue}
)

// logAt logs text at position and returns the handler's output.
func logAt(t *testing.T, position plainPosition, text string) string {
	t.Helper()

	var buf bytes.Buffer

	handler := slog.Handler(newTestHandler(&buf, slog.LevelInfo))
	message := "msg"

	var attrs []slog.Attr

	switch position {
	case positionMessage:
		message = text
		attrs = []slog.Attr{slog.Int("after", 1)}
	case positionTopKey:
		attrs = []slog.Attr{slog.String(text, "v")}
	case positionGroupName:
		handler = handler.WithGroup(text)
		attrs = []slog.Attr{slog.String("k", "v")}
	case positionGroupChildKey:
		attrs = []slog.Attr{slog.Group("g", slog.String(text, "v"))}
	case positionStringValue:
		attrs = []slog.Attr{slog.String("k", text)}
	case positionErrorValue:
		attrs = []slog.Attr{slog.Any("err", textError(text))}
	case positionGroupValue:
		attrs = []slog.Attr{slog.Group("g", slog.String("k", text))}
	}

	err := handler.Handle(context.Background(), makeRecord(slog.LevelInfo, message, attrs...))
	if err != nil {
		t.Fatal(err)
	}

	return buf.String()
}

// wantAt returns the exact line and the fields logAt(position, text) must produce, from the
// escaped message and the quoted form of text.
func wantAt(position plainPosition, text, message, quoted string) (string, []plainField) {
	// quoted is strconv-quoted text: splice a prefix or a suffix inside its quotes.
	inQuotes := func(prefix, suffix string) string {
		return `"` + prefix + quoted[1:len(quoted)-1] + suffix + `"`
	}

	switch position {
	case positionMessage:
		return "level=INFO " + message + " after=1\n", []plainField{{key: "after", value: "1"}}
	case positionTopKey:
		return "level=INFO msg " + quoted + "=v\n", []plainField{{key: text, value: "v"}}
	case positionGroupName:
		return "level=INFO msg " + inQuotes(
				"",
				".k",
			) + "=v\n", []plainField{
				{key: text + ".k", value: "v"},
			}
	case positionGroupChildKey:
		return "level=INFO msg g={" + inQuotes("g.", "") + "=v}\n",
			[]plainField{{key: "g", group: true}, {key: "g." + text, value: "v"}}
	case positionStringValue:
		return "level=INFO msg k=" + quoted + "\n", []plainField{{key: "k", value: text}}
	case positionErrorValue:
		return "level=INFO msg err=" + quoted + "\n", []plainField{{key: "err", value: text}}
	case positionGroupValue:
		return "level=INFO msg g={g.k=" + quoted + "}\n",
			[]plainField{{key: "g", group: true}, {key: "g.k", value: text}}
	}

	return "", nil
}

// textError is an error whose text is exactly its value.
type textError string

func (e textError) Error() string {
	return string(e)
}

// TestPlainHandler_Escaping puts hostile text in every position of a plain line. Each line must
// be exactly one line of valid UTF-8, match the exact escaped text, and parse back to exactly
// the expected fields: nothing can end a record or inject a field.
func TestPlainHandler_Escaping(t *testing.T) {
	cases := []struct {
		name string
		text string
		// message is the escaped form in the unquoted message; quoted, the form in a key or a
		// value (strconv.Quote).
		message   string
		quoted    string
		positions []plainPosition
	}{
		{
			name:    "forged record",
			text:    "\nlevel=ERROR forged",
			message: `\nlevel=ERROR forged`,
			quoted:  `"\nlevel=ERROR forged"`,
			// WithGroup trims the leading newline of a group name.
			positions: textPositions,
		},
		{
			name:      "ESC",
			text:      "\x1b[31m",
			message:   `\x1b[31m`,
			quoted:    `"\x1b[31m"`,
			positions: allPositions(),
		},
		{
			name:      "U+0085",
			text:      "a\u0085b",
			message:   `a\u0085b`,
			quoted:    `"a\u0085b"`,
			positions: allPositions(),
		},
		{
			name:      "U+009B",
			text:      "a\u009bb",
			message:   `a\u009bb`,
			quoted:    `"a\u009bb"`,
			positions: allPositions(),
		},
		{
			name:      "U+2028",
			text:      "a\u2028b",
			message:   `a\u2028b`,
			quoted:    `"a\u2028b"`,
			positions: allPositions(),
		},
		{
			name:      "U+2029",
			text:      "a\u2029b",
			message:   `a\u2029b`,
			quoted:    `"a\u2029b"`,
			positions: allPositions(),
		},
		{
			name:      "bidi override",
			text:      "a\u202eb",
			message:   `a\u202eb`,
			quoted:    `"a\u202eb"`,
			positions: allPositions(),
		},
		{
			name:      "invalid byte",
			text:      "a\xffb",
			message:   `a\xffb`,
			quoted:    `"a\xffb"`,
			positions: allPositions(),
		},
		{
			name:      "quote without a space",
			text:      `a"b`,
			message:   `a"b`,
			quoted:    `"a\"b"`,
			positions: allPositions(),
		},
		{
			name:      "newline without a space",
			text:      "a\nb",
			message:   `a\nb`,
			quoted:    `"a\nb"`,
			positions: allPositions(),
		},
		{
			name:      "backslash",
			text:      `a\b`,
			message:   `a\\b`,
			quoted:    `"a\\b"`,
			positions: allPositions(),
		},
		{
			name:      "error with spaces",
			text:      "dial tcp: connection refused",
			quoted:    `"dial tcp: connection refused"`,
			positions: []plainPosition{positionErrorValue},
		},
		{name: "empty value", text: "", quoted: `""`, positions: valuePositions},
		{name: "empty key", text: "", quoted: `""`, positions: []plainPosition{positionTopKey}},
		{
			name:      "key with a space and =",
			text:      "x level=ERROR",
			quoted:    `"x level=ERROR"`,
			positions: keyPositions,
		},
		{name: "key with a quote", text: `a"b`, quoted: `"a\"b"`, positions: keyPositions},
		{name: "key with a close brace", text: "a}b", quoted: `"a}b"`, positions: keyPositions},
		{name: "key with an open brace", text: "a{b", quoted: `"a{b"`, positions: keyPositions},
		{
			name:      "group name",
			text:      "g h=1",
			quoted:    `"g h=1"`,
			positions: []plainPosition{positionGroupName},
		},
		{name: "close brace value", text: "}", quoted: `"}"`, positions: valuePositions},
		{name: "open brace value", text: "{", quoted: `"{"`, positions: valuePositions},
	}

	for _, testCase := range cases {
		for _, position := range testCase.positions {
			t.Run(testCase.name+"/"+plainPositionNames[position], func(t *testing.T) {
				got := logAt(t, position, testCase.text)
				wantLine, wantFields := wantAt(
					position,
					testCase.text,
					testCase.message,
					testCase.quoted,
				)

				if strings.Count(got, "\n") != 1 || !strings.HasSuffix(got, "\n") {
					t.Errorf("output is not exactly one line: %q", got)
				}

				if !utf8.ValidString(got) {
					t.Errorf("output is not valid UTF-8: %q", got)
				}

				if got != wantLine {
					t.Errorf("got  %q\nwant %q", got, wantLine)
				}

				// The fields start after the level and the message.
				prefix := "level=INFO msg"
				if position == positionMessage {
					prefix = "level=INFO " + testCase.message
				}

				fields, found := strings.CutPrefix(strings.TrimSuffix(got, "\n"), prefix)
				if !found {
					t.Fatalf("line %q does not start with %q", got, prefix)
				}

				if parsed := parsePlainFields(t, fields); !slices.Equal(parsed, wantFields) {
					t.Errorf("fields = %+v, want %+v", parsed, wantFields)
				}
			})
		}
	}
}

// allPositions is every position except the WithGroup name, which slog trims of spaces.
func allPositions() []plainPosition {
	return append(slices.Clone(textPositions), positionGroupName)
}
