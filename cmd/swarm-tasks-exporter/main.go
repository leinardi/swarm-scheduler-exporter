// Package main wires and runs the exporter binary.
// It owns CLI flag parsing, logging setup, and the HTTP server with timeouts.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"net/http"
	"os"
	"time"

	"github.com/docker/docker/client"
	"github.com/leinardi/swarm-tasks-exporter/internal/collector"
	labelutil "github.com/leinardi/swarm-tasks-exporter/internal/labels"
	"github.com/leinardi/swarm-tasks-exporter/internal/server"
	"github.com/sirupsen/logrus"
)

const (
	// DefaultPollDelay is the default interval between polls.
	DefaultPollDelay = 10 * time.Second
)

// configureLogger applies the selected log formatter and level.
// This keeps logging concerns isolated from business logic.
func configureLogger() {
	switch *logFormat {
	case "text":
		logrus.SetFormatter(new(logrus.TextFormatter))
	case "json":
		logrus.SetFormatter(new(logrus.JSONFormatter))
	default:
		_, _ = fmt.Fprintf(
			os.Stderr,
			"Invalid log format %q. Should be either json or text.\n",
			*logFormat,
		)

		os.Exit(1)
	}

	switch *logLevel {
	case "debug":
		logrus.SetLevel(logrus.DebugLevel)
	case "info":
		logrus.SetLevel(logrus.InfoLevel)
	case "warn":
		logrus.SetLevel(logrus.WarnLevel)
	case "error":
		logrus.SetLevel(logrus.ErrorLevel)
	case "fatal":
		logrus.SetLevel(logrus.FatalLevel)
	case "panic":
		logrus.SetLevel(logrus.PanicLevel)
	default:
		_, _ = fmt.Fprintf(
			os.Stderr,
			"Invalid log level %q. Should be either debug, info, warn, error, fatal, panic.\n",
			*logLevel,
		)

		os.Exit(1)
	}
}

// stringSlice implements flag.Value to support repeated -label flags
// (e.g., -label team -label tier). Each call to Set appends a value.
type stringSlice []string

// ErrEmptyFlagValue is returned when a repeated flag (like -label)
// is provided with an empty value. This is a sentinel error for tests
// and for clearer calling code.
var ErrEmptyFlagValue = errors.New("empty flag value")

// String returns the flag value in a human-friendly form.
func (i *stringSlice) String() string {
	return fmt.Sprint(*i)
}

// Set implements flag.Value for stringSlice by appending non-empty values.
func (i *stringSlice) Set(value string) error {
	if value == "" {
		return ErrEmptyFlagValue
	}

	*i = append(*i, value)

	return nil
}

var (
	// CLI flags.
	listenAddr = flag.String("listen-addr", "0.0.0.0:8888", "IP address and port to bind")
	pollDelay  = flag.Duration("poll-delay", DefaultPollDelay, "Delay in seconds between two polls")
	logFormat  = flag.String("log-format", "text", "Either json or text")
	logLevel   = flag.String("log-level", "info", "Either debug, info, warn, error, fatal, panic")
	help       = flag.Bool("help", false, "Display help message")

	customLabels stringSlice
)

// usage prints flag usage to stdout. We avoid fmt.Print* linters by
// writing to an explicit writer and by setting the flag package's output.
func usage() {
	w := os.Stdout
	_, _ = fmt.Fprintf(w, "Usage of %s:\n", os.Args[0])
	flag.CommandLine.SetOutput(w)
	flag.PrintDefaults()
}

// main initializes logging, Prometheus collectors, the Docker client,
// starts the polling and events goroutines, and serves /metrics with timeouts.
func main() {
	flag.Var(&customLabels, "label", "Name of custom service labels to add to metrics")
	flag.Parse()

	if *help {
		usage()
		os.Exit(0)
	}

	configureLogger()

	// Log version info for diagnostics and to keep ldflags-injected vars "used".
	logrus.Infof(
		"swarm-tasks-exporter starting… version=%s commit=%s date=%s",
		version,
		commit,
		date,
	)

	// Validate and sanitize requested custom labels (+ guard count)
	countErr := labelutil.ValidateCustomLabelCount(len(customLabels))
	if countErr != nil {
		logrus.Fatal(countErr)
	}

	sanitized, sanitizeErr := labelutil.ValidateAndSanitizeLabelNames([]string(customLabels))
	if sanitizeErr != nil {
		logrus.Fatal(sanitizeErr)
	}

	// Store both raw and sanitized forms so we can look up by raw and emit by sanitized.
	collector.SetCustomLabels([]string(customLabels), sanitized)
	collector.ConfigureDesiredReplicasGauge()
	collector.ConfigureReplicasStateGauge()

	ctx := context.Background()

	// Docker client is configured from environment variables (DOCKER_HOST, etc.).
	cli, err := client.NewClientWithOpts(client.FromEnv)
	if err != nil {
		logrus.Fatal(err)
	}
	defer cli.Close()

	cli.NegotiateAPIVersion(ctx)

	// Goroutine #1: initial gauge fill + event listener.
	go func() {
		initErr := collector.InitDesiredReplicasGauge(ctx, cli)
		if initErr != nil {
			logrus.Fatal(initErr)
		}

		listenErr := collector.ListenSwarmEvents(ctx, cli)
		if listenErr != nil {
			logrus.Fatal(listenErr)
		}
	}()

	// Goroutine #2: periodic task-state polling to refresh per-state gauges.
	go func() {
		logrus.Info("Start polling replicas state every ", *pollDelay)

		for {
			logrus.Info("Polling replicas state...")

			polled, pollErr := collector.PollReplicasState(ctx, cli)
			if pollErr != nil {
				logrus.Error(pollErr)
			} else {
				collector.UpdateReplicasStateGauge(polled)
			}

			time.Sleep(*pollDelay)
		}
	}()

	// HTTP server with sane timeouts to satisfy gosec G114 and production best practice.
	mux := server.NewMux()

	logrus.Infof("Start HTTP server on %q.", *listenAddr)

	var srv http.Server

	srv.Addr = *listenAddr
	srv.Handler = mux
	srv.ReadHeaderTimeout = 5 * time.Second
	srv.ReadTimeout = 10 * time.Second
	srv.WriteTimeout = 15 * time.Second
	srv.IdleTimeout = 60 * time.Second

	err = srv.ListenAndServe()
	if err != nil && !errors.Is(err, http.ErrServerClosed) {
		logrus.Fatalf("http server error: %v", err)
	}
}
