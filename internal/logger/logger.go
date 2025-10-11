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
		handler := slog.NewTextHandler(os.Stdout, &slog.HandlerOptions{Level: slog.LevelInfo})
		globalLogger = slog.New(handler)
	}

	return globalLogger
}

// Set replaces the global logger (primarily for tests or custom wiring).
func Set(l *slog.Logger) {
	globalLogger = l
}

// Configure builds and installs a slog.Logger based on CLI flags.
// format: "json" or "text" (default text if unknown)
// level:  "debug", "info", "warn", "error", "fatal", "panic" (fatal/panic map to error)
func Configure(format, level string) *slog.Logger {
	sanitizedLevel := parseLevel(level)
	handler := buildHandler(format, sanitizedLevel)
	logger := slog.New(handler)
	Set(logger)

	return logger
}

func buildHandler(format string, level slog.Level) slog.Handler {
	opts := &slog.HandlerOptions{
		Level:     level,
		AddSource: false,
	}

	switch strings.ToLower(format) {
	case "json":
		return slog.NewJSONHandler(os.Stdout, opts)
	default: // "text" or unknown
		return slog.NewTextHandler(os.Stdout, opts)
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
