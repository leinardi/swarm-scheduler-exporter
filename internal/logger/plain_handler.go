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
	out         io.Writer
	leveler     slog.Leveler
	includeTime bool

	// state captured by With/WithGroup
	prefixAttrs []slog.Attr
	groups      []string

	// single-writer lock to avoid interleaving — pointer so copies share the same lock
	mu *sync.Mutex
}

func newPlainTextHandler(out io.Writer, level slog.Leveler, includeTime bool) *PlainTextHandler {
	if out == nil {
		out = os.Stdout
	}

	if level == nil {
		level = slog.LevelInfo
	}

	return &PlainTextHandler{
		out:         out,
		leveler:     level,
		includeTime: includeTime,
		mu:          &sync.Mutex{},
	}
}

func (h *PlainTextHandler) Enabled(_ context.Context, level slog.Level) bool {
	return level >= h.leveler.Level()
}

// Handle writes a single log record in the "plain" format.
//
//nolint:gocritic // slog.Handler requires slog.Record by value; cannot change the signature.
func (h *PlainTextHandler) Handle(_ context.Context, rec slog.Record) error {
	var buf bytes.Buffer

	// Optional time first (same position as TextHandler)
	if h.includeTime && !rec.Time.IsZero() {
		buf.WriteString("time=")
		buf.WriteString(rec.Time.Format(time.RFC3339Nano))
		buf.WriteByte(' ')
	}

	// Level
	buf.WriteString("level=")
	buf.WriteString(levelToUpper(rec.Level))

	// Message as raw text, WITHOUT msg= wrapper
	if rec.Message != "" {
		buf.WriteByte(' ')
		buf.WriteString(rec.Message)
	}

	// Pre-resolved prefix attrs (from With)
	if len(h.prefixAttrs) > 0 {
		for i := range h.prefixAttrs {
			writeAttrKV(&buf, qualify(h.groups, h.prefixAttrs[i]))
		}
	}

	// Record attrs
	if rec.NumAttrs() > 0 {
		rec.Attrs(func(attr slog.Attr) bool {
			writeAttrKV(&buf, qualify(h.groups, attr))

			return true
		})
	}

	// Final newline
	buf.WriteByte('\n')

	h.mu.Lock()
	_, writeErr := h.out.Write(buf.Bytes())
	h.mu.Unlock()

	if writeErr != nil {
		return fmt.Errorf("plain handler write: %w", writeErr)
	}

	return nil
}

func (h *PlainTextHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	if len(attrs) == 0 {
		return h
	}

	// Copy-on-write to keep original immutable
	cp := *h // safe: mu is a pointer so lock isn't copied
	cp.prefixAttrs = append(append([]slog.Attr(nil), h.prefixAttrs...), attrs...)

	return &cp
}

func (h *PlainTextHandler) WithGroup(name string) slog.Handler {
	name = strings.TrimSpace(name)
	if name == "" {
		return h
	}

	cp := *h // safe: mu is a pointer so lock isn't copied
	cp.groups = append(append([]string(nil), h.groups...), name)

	return &cp
}

// --- helpers ---

func levelToUpper(levelVal slog.Level) string {
	switch {
	case levelVal <= slog.LevelDebug:
		return "DEBUG"
	case levelVal == slog.LevelInfo:
		return "INFO"
	case levelVal == slog.LevelWarn:
		return "WARN"
	default:
		return "ERROR"
	}
}

// qualify applies nested group prefixes (e.g., group1.group2.key).
func qualify(groups []string, attr slog.Attr) slog.Attr {
	attr.Value = attr.Value.Resolve()
	if len(groups) == 0 {
		return attr
	}

	qualified := strings.Join(groups, ".")
	if attr.Key != "" {
		qualified += "." + attr.Key
	}

	return slog.Attr{Key: qualified, Value: attr.Value}
}

// writeAttrKV writes: " key=value" (note the leading space).
func writeAttrKV(buf *bytes.Buffer, attr slog.Attr) {
	// Skip empty attrs
	if attr.Equal(slog.Attr{}) || attr.Key == "" {
		return
	}

	writeKV(buf, attr.Key, attr.Value.Resolve(), true /*leadingSpace*/)
}

// emitKVInsideBraces writes: "key=value" (NO leading space).
func emitKVInsideBraces(buf *bytes.Buffer, attr slog.Attr) {
	// Skip empty attrs
	if attr.Equal(slog.Attr{}) || attr.Key == "" {
		return
	}

	writeKV(buf, attr.Key, attr.Value.Resolve(), false /*leadingSpace*/)
}

// writeKV is the single entry point that writes a key/value pair, optionally
// with a leading space. It handles all slog kinds, including groups.
// This consolidates the logic used by both writeAttrKV and emitKVInsideBraces.
func writeKV(buf *bytes.Buffer, key string, val slog.Value, leadingSpace bool) {
	if leadingSpace {
		buf.WriteByte(' ')
	}

	writeKeyEq(buf, key)

	switch val.Kind() {
	case slog.KindString,
		slog.KindInt64,
		slog.KindUint64,
		slog.KindFloat64,
		slog.KindBool,
		slog.KindTime,
		slog.KindDuration,
		slog.KindAny:
		writeScalarValue(buf, val)

	case slog.KindLogValuer:
		// Resolve and re-emit
		resolved := val.Resolve()
		writeKV(buf, key, resolved, leadingSpace) // recurse on resolved kind

	case slog.KindGroup:
		groupAttrs := val.Group()
		// Empty group -> {}
		writeGroupBraced(buf, key, groupAttrs)

	default:
		// Future-proof fallback
		fmt.Fprint(buf, val.Any())
	}
}

// writeKeyEq writes "key=" without any whitespace decisions.
func writeKeyEq(buf *bytes.Buffer, key string) {
	buf.WriteString(key)
	buf.WriteByte('=')
}

// writeScalarValue writes non-group kinds.
func writeScalarValue(buf *bytes.Buffer, val slog.Value) {
	switch val.Kind() {
	case slog.KindString:
		str := val.String()
		if strings.ContainsAny(str, " \t") {
			buf.WriteByte('"')
			buf.WriteString(strings.ReplaceAll(str, `"`, `\"`))
			buf.WriteByte('"')
		} else {
			buf.WriteString(str)
		}
	case slog.KindInt64:
		buf.WriteString(strconv.FormatInt(val.Int64(), 10))
	case slog.KindUint64:
		buf.WriteString(strconv.FormatUint(val.Uint64(), 10))
	case slog.KindFloat64:
		buf.WriteString(strconv.FormatFloat(val.Float64(), 'g', -1, 64))
	case slog.KindBool:
		if val.Bool() {
			buf.WriteString("true")
		} else {
			buf.WriteString("false")
		}
	case slog.KindTime:
		t := val.Time()
		if t.IsZero() {
			buf.WriteString("0")
		} else {
			buf.WriteString(t.Format(time.RFC3339Nano))
		}
	case slog.KindDuration:
		buf.WriteString(val.Duration().String())
	case slog.KindAny:
		fmt.Fprint(buf, val.Any())
	case slog.KindLogValuer:
		// Resolve and re-emit via scalar path.
		writeScalarValue(buf, val.Resolve())
	case slog.KindGroup:
		// Groups are handled by callers; emit a compact placeholder here.
		buf.WriteString("{}")
	default:
		// Should not happen (callers route other kinds), keep safe:
		fmt.Fprint(buf, val.Any())
	}
}

// writeGroupBraced writes group as: key={a=1 b="two"} with child keys flattened
// under the parent key (e.g., key.a=..., key.b=...).
func writeGroupBraced(buf *bytes.Buffer, parentKey string, groupAttrs []slog.Attr) {
	// We've already written "key=" outside; now write braces content.
	if len(groupAttrs) == 0 {
		buf.WriteString("{}")

		return
	}

	buf.WriteByte('{')

	for i := range groupAttrs {
		if i > 0 {
			buf.WriteByte(' ')
		}

		child := groupAttrs[i]

		qualifiedKey := parentKey
		if child.Key != "" {
			qualifiedKey += "." + child.Key
		}

		emitKVInsideBraces(buf, slog.Attr{Key: qualifiedKey, Value: child.Value.Resolve()})
	}

	buf.WriteByte('}')
}
