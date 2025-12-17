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
	"fmt"
	"io"
	"log/slog"
	"os"
	"strconv"
	"strings"
	"sync"
	"time"
)

// PlainTextHandler writes lines like:
//
//	level=INFO hello world k=v foo=bar
//
// If includeTime is true, it prefixes: time=... level=INFO ...
type PlainTextHandler struct {
	outputWriter io.Writer
	leveler      slog.Leveler
	includeTime  bool

	// state captured by With/WithGroup
	prefixAttributes []slog.Attr
	groups           []string

	// single-writer lock to avoid interleaving — pointer so copies share the same lock
	mutex *sync.Mutex
}

func newPlainTextHandler(output io.Writer, level slog.Leveler, includeTime bool) *PlainTextHandler {
	if output == nil {
		output = os.Stdout
	}

	if level == nil {
		level = slog.LevelInfo
	}

	return &PlainTextHandler{
		outputWriter:     output,
		leveler:          level,
		includeTime:      includeTime,
		mutex:            &sync.Mutex{},
		prefixAttributes: nil,
		groups:           nil,
	}
}

func (handler *PlainTextHandler) Enabled(_ context.Context, level slog.Level) bool {
	return level >= handler.leveler.Level()
}

// Handle writes a single log record in the "plain" format.
//
//nolint:gocritic // slog.Handler requires slog.Record by value; cannot change the signature.
func (handler *PlainTextHandler) Handle(_ context.Context, record slog.Record) error {
	var buffer bytes.Buffer

	// Optional time first (same position as TextHandler)
	if handler.includeTime && !record.Time.IsZero() {
		buffer.WriteString("time=")
		buffer.WriteString(record.Time.Format(time.RFC3339Nano))
		buffer.WriteByte(' ')
	}

	// Level
	buffer.WriteString("level=")
	buffer.WriteString(levelToUpper(record.Level))

	// Message as raw text, WITHOUT msg= wrapper
	if record.Message != "" {
		buffer.WriteByte(' ')
		buffer.WriteString(record.Message)
	}

	// Pre-resolved prefix attrs (from With)
	if len(handler.prefixAttributes) > 0 {
		for index := range handler.prefixAttributes {
			writeAttrKV(&buffer, qualify(handler.groups, handler.prefixAttributes[index]))
		}
	}

	// Record attrs
	if record.NumAttrs() > 0 {
		record.Attrs(func(attribute slog.Attr) bool {
			writeAttrKV(&buffer, qualify(handler.groups, attribute))

			return true
		})
	}

	// Final newline
	buffer.WriteByte('\n')

	handler.mutex.Lock()
	_, writeErr := handler.outputWriter.Write(buffer.Bytes())
	handler.mutex.Unlock()

	if writeErr != nil {
		return fmt.Errorf("plain handler write: %w", writeErr)
	}

	return nil
}

func (handler *PlainTextHandler) WithAttrs(attributes []slog.Attr) slog.Handler {
	if len(attributes) == 0 {
		return handler
	}

	// Copy-on-write to keep original immutable
	copyHandler := *handler // safe: mutex is a pointer so lock isn't copied
	copyHandler.prefixAttributes = append(
		append([]slog.Attr(nil), handler.prefixAttributes...),
		attributes...)

	return &copyHandler
}

func (handler *PlainTextHandler) WithGroup(name string) slog.Handler {
	name = strings.TrimSpace(name)
	if name == "" {
		return handler
	}

	copyHandler := *handler // safe: mutex is a pointer so lock isn't copied
	copyHandler.groups = append(append([]string(nil), handler.groups...), name)

	return &copyHandler
}

// --- helpers ---

func levelToUpper(levelValue slog.Level) string {
	switch {
	case levelValue <= slog.LevelDebug:
		return "DEBUG"
	case levelValue == slog.LevelInfo:
		return "INFO"
	case levelValue == slog.LevelWarn:
		return "WARN"
	default:
		return "ERROR"
	}
}

// qualify applies nested group prefixes (e.g., group1.group2.key).
func qualify(groups []string, attribute slog.Attr) slog.Attr {
	attribute.Value = attribute.Value.Resolve()
	if len(groups) == 0 {
		return attribute
	}

	qualified := strings.Join(groups, ".")
	if attribute.Key != "" {
		qualified += "." + attribute.Key
	}

	return slog.Attr{Key: qualified, Value: attribute.Value}
}

// writeAttrKV writes: " key=value" (note the leading space).
func writeAttrKV(buffer *bytes.Buffer, attribute slog.Attr) {
	// Skip empty attrs
	if attribute.Equal(slog.Attr{}) || attribute.Key == "" {
		return
	}

	writeKV(buffer, attribute.Key, attribute.Value.Resolve(), true /* includeLeadingSpace */)
}

// emitKVInsideBraces writes: "key=value" (NO leading space).
func emitKVInsideBraces(buffer *bytes.Buffer, attribute slog.Attr) {
	// Skip empty attrs
	if attribute.Equal(slog.Attr{}) || attribute.Key == "" {
		return
	}

	writeKV(buffer, attribute.Key, attribute.Value.Resolve(), false /* includeLeadingSpace */)
}

// writeKV is the single entry point that writes a key/value pair, optionally
// with a leading space. It handles all slog kinds, including groups.
// This consolidates the logic used by both writeAttrKV and emitKVInsideBraces.
func writeKV(buffer *bytes.Buffer, key string, value slog.Value, includeLeadingSpace bool) {
	if includeLeadingSpace {
		buffer.WriteByte(' ')
	}

	writeKeyEq(buffer, key)

	switch value.Kind() {
	case slog.KindString,
		slog.KindInt64,
		slog.KindUint64,
		slog.KindFloat64,
		slog.KindBool,
		slog.KindTime,
		slog.KindDuration,
		slog.KindAny:
		writeScalarValue(buffer, value)

	case slog.KindLogValuer:
		// Resolve and re-emit
		resolved := value.Resolve()
		writeKV(buffer, key, resolved, includeLeadingSpace) // recurse on resolved kind

	case slog.KindGroup:
		groupAttributes := value.Group()
		// Empty group -> {}
		writeGroupBraced(buffer, key, groupAttributes)

	default:
		// Future-proof fallback
		fmt.Fprint(buffer, value.Any())
	}
}

// writeKeyEq writes "key=" without any whitespace decisions.
func writeKeyEq(buffer *bytes.Buffer, key string) {
	buffer.WriteString(key)
	buffer.WriteByte('=')
}

// writeScalarValue writes non-group kinds.
func writeScalarValue(buffer *bytes.Buffer, value slog.Value) {
	switch value.Kind() {
	case slog.KindString:
		text := value.String()
		if strings.ContainsAny(text, " \t") {
			buffer.WriteByte('"')
			buffer.WriteString(strings.ReplaceAll(text, `"`, `\"`))
			buffer.WriteByte('"')
		} else {
			buffer.WriteString(text)
		}
	case slog.KindInt64:
		buffer.WriteString(strconv.FormatInt(value.Int64(), 10))
	case slog.KindUint64:
		buffer.WriteString(strconv.FormatUint(value.Uint64(), 10))
	case slog.KindFloat64:
		buffer.WriteString(strconv.FormatFloat(value.Float64(), 'g', -1, 64))
	case slog.KindBool:
		if value.Bool() {
			buffer.WriteString("true")
		} else {
			buffer.WriteString("false")
		}
	case slog.KindTime:
		timestamp := value.Time()
		if timestamp.IsZero() {
			buffer.WriteString("0")
		} else {
			buffer.WriteString(timestamp.Format(time.RFC3339Nano))
		}
	case slog.KindDuration:
		buffer.WriteString(value.Duration().String())
	case slog.KindAny:
		fmt.Fprint(buffer, value.Any())
	case slog.KindLogValuer:
		// Resolve and re-emit via scalar path.
		writeScalarValue(buffer, value.Resolve())
	case slog.KindGroup:
		// Groups are handled by callers; emit a compact placeholder here.
		buffer.WriteString("{}")
	default:
		// Should not happen (callers route other kinds), keep safe:
		fmt.Fprint(buffer, value.Any())
	}
}

// writeGroupBraced writes group as: key={a=1 b="two"} with child keys flattened
// under the parent key (e.g., key.a=..., key.b=...).
func writeGroupBraced(buffer *bytes.Buffer, parentKey string, groupAttributes []slog.Attr) {
	// We've already written "key=" outside; now write braces content.
	if len(groupAttributes) == 0 {
		buffer.WriteString("{}")

		return
	}

	buffer.WriteByte('{')

	for index := range groupAttributes {
		if index > 0 {
			buffer.WriteByte(' ')
		}

		child := groupAttributes[index]

		qualifiedKey := parentKey
		if child.Key != "" {
			qualifiedKey += "." + child.Key
		}

		emitKVInsideBraces(buffer, slog.Attr{Key: qualifiedKey, Value: child.Value.Resolve()})
	}

	buffer.WriteByte('}')
}
