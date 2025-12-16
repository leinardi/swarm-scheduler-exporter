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
	"os"
	"strings"
	"sync"
)

// globalLogger holds the configured slog.Logger.
// Access it with L() and set it with Set()/Configure().
var globalLogger *slog.Logger

// initOnce ensures the default logger is initialized exactly once.
var initOnce sync.Once

// L returns the configured slog.Logger. If Configure/Set hasn't been called yet,
// it returns a reasonable default text logger at INFO level to avoid nil panics.
func L() *slog.Logger {
	initOnce.Do(func() {
		if globalLogger == nil {
			handler := slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{
				Level: slog.LevelInfo,
			})
			globalLogger = slog.New(handler)
		}
	})

	return globalLogger
}

// Set replaces the global logger (primarily for tests or custom wiring).
func Set(newLogger *slog.Logger) {
	globalLogger = newLogger
}

// Configure builds and installs a slog.Logger based on CLI flags.
// format: "json" or "text" (unknown -> text)
// level:  "debug", "info", "warn", "error", "fatal", "panic" (fatal/panic -> error)
// includeTime: if false, the time attribute is removed from log records.
func Configure(format, level string, includeTime bool) *slog.Logger {
	logLevel := parseLevel(level)

	var handler slog.Handler

	switch strings.ToLower(format) {
	case "json":
		handler = slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{
			Level:       logLevel,
			ReplaceAttr: timeStripper(includeTime),
		})
	case "plain":
		handler = newPlainTextHandler(os.Stdout, logLevel, includeTime)
	default: // "text"
		handler = slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{
			Level:       logLevel,
			ReplaceAttr: timeStripper(includeTime),
		})
	}

	configured := slog.New(handler)
	Set(configured)

	return configured
}

// timeStripper returns a ReplaceAttr function that removes the time attribute
// when includeTime is false. When includeTime is true, it returns nil (no-op).
func timeStripper(includeTime bool) func([]string, slog.Attr) slog.Attr {
	if includeTime {
		return nil
	}

	return func(_ []string, attr slog.Attr) slog.Attr {
		if attr.Key == slog.TimeKey {
			return slog.Attr{} // drop time
		}

		return attr
	}
}

// parseLevel converts a string level to slog.Level.
// Unknown inputs default to INFO; "fatal"/"panic" are treated as ERROR.
func parseLevel(level string) slog.Level {
	switch strings.ToLower(level) {
	case "debug":
		return slog.LevelDebug
	case "info":
		return slog.LevelInfo
	case "warn", "warning":
		return slog.LevelWarn
	case "error", "fatal", "panic":
		return slog.LevelError
	default:
		return slog.LevelInfo
	}
}
