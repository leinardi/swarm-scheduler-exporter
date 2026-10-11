/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
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

package testenv

import (
	"context"
	"fmt"
	"io"
	"log/slog"
	"strings"
)

// Secret wraps a sensitive string, such as a registry token. A redacting logger replaces it with a
// placeholder, and so does fmt.
type Secret string

// String returns the placeholder, so a Secret formatted by mistake does not leak.
func (Secret) String() string { return redacted }

// GoString returns the placeholder for %#v as well.
func (Secret) GoString() string { return redacted }

const redacted = "[redacted]"

// NewRedactingLogger returns a text logger that replaces any attribute keyed "secret", any Secret
// value, and the value of every KEY=VALUE entry of a []string keyed "env" (a container's Env) with
// a placeholder, keeping the key so the log still says what was set.
func NewRedactingLogger(out io.Writer) *slog.Logger {
	handler := slog.NewTextHandler(out, &slog.HandlerOptions{Level: slog.LevelDebug})

	return slog.New(&redactingHandler{inner: handler})
}

type redactingHandler struct {
	inner slog.Handler
}

func (h *redactingHandler) Enabled(ctx context.Context, level slog.Level) bool {
	return h.inner.Enabled(ctx, level)
}

//nolint:gocritic // hugeParam: slog.Record is large, but the signature is fixed by slog.Handler.
func (h *redactingHandler) Handle(ctx context.Context, record slog.Record) error {
	cleaned := slog.NewRecord(record.Time, record.Level, record.Message, record.PC)
	record.Attrs(func(attr slog.Attr) bool {
		cleaned.AddAttrs(redactAttr(attr))

		return true
	})

	err := h.inner.Handle(ctx, cleaned)
	if err != nil {
		return fmt.Errorf("inner slog handler: %w", err)
	}

	return nil
}

func (h *redactingHandler) WithAttrs(attrs []slog.Attr) slog.Handler {
	cleaned := make([]slog.Attr, len(attrs))
	for index, attr := range attrs {
		cleaned[index] = redactAttr(attr)
	}

	return &redactingHandler{inner: h.inner.WithAttrs(cleaned)}
}

func (h *redactingHandler) WithGroup(name string) slog.Handler {
	return &redactingHandler{inner: h.inner.WithGroup(name)}
}

func redactAttr(attr slog.Attr) slog.Attr {
	if attr.Key == "secret" {
		return slog.String(attr.Key, redacted)
	}

	switch value := attr.Value.Any().(type) {
	case Secret:
		return slog.String(attr.Key, redacted)
	case []string:
		if attr.Key == "env" {
			return slog.Any(attr.Key, RedactEnv(value))
		}
	}

	return attr
}

// RedactEnv returns KEY=VALUE entries safe to log: each value is replaced, each key kept. An entry
// without "=" is replaced whole, since it cannot be told apart from a bare value.
func RedactEnv(env []string) []string {
	cleaned := make([]string, len(env))

	for index, entry := range env {
		key, _, found := strings.Cut(entry, "=")
		if !found {
			cleaned[index] = redacted

			continue
		}

		cleaned[index] = key + "=" + redacted
	}

	return cleaned
}
