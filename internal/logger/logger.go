package logger

import (
	"log/slog"
	"os"
	"strings"
)

// globalLogger holds the configured slog.Logger.
// Access it with L() and set it with Set()/Configure().
var globalLogger *slog.Logger

// L returns the configured slog.Logger. If Configure/Set hasn't been called yet,
// it returns a reasonable default text logger at INFO level to avoid nil panics.
func L() *slog.Logger {
	if globalLogger == nil {
		handler := slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{
			Level: slog.LevelInfo,
		})
		globalLogger = slog.New(handler)
	}

	return globalLogger
}

// Set replaces the global logger (primarily for tests or custom wiring).
func Set(l *slog.Logger) {
	globalLogger = l
}

// Configure builds and installs a slog.Logger based on CLI flags.
// format: "json" or "text" (unknown -> text)
// level:  "debug", "info", "warn", "error", "fatal", "panic" (fatal/panic -> error)
// includeTime: if false, the time attribute is removed from log records.
func Configure(format, level string, includeTime bool) *slog.Logger {
	lvl := parseLevel(level)

	var handler slog.Handler

	switch strings.ToLower(format) {
	case "json":
		handler = slog.NewJSONHandler(os.Stdout, &slog.HandlerOptions{
			Level:       lvl,
			ReplaceAttr: timeStripper(includeTime),
		})
	case "plain":
		handler = newPlainTextHandler(os.Stdout, lvl, includeTime)
	default: // "text"
		handler = slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{
			Level:       lvl,
			ReplaceAttr: timeStripper(includeTime),
		})
	}

	logger := slog.New(handler)
	Set(logger)

	return logger
}

func timeStripper(includeTime bool) func([]string, slog.Attr) slog.Attr {
	if includeTime {
		return nil
	}

	return func(_ []string, a slog.Attr) slog.Attr {
		if a.Key == slog.TimeKey {
			return slog.Attr{} // drop time
		}

		return a
	}
}

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
