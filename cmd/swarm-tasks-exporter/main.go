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
	"os/signal"
	"sync"
	"syscall"
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

	// Operability constants.
	minPollDelay        = 1 * time.Second
	httpShutdownTimeout = 10 * time.Second
	healthTickInterval  = 5 * time.Second
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
	pollDelay  = flag.Duration(
		"poll-delay",
		DefaultPollDelay,
		"How often to poll tasks (Go duration, e.g. 10s, 1m). Minimum 1s.",
	)
	logFormat = flag.String("log-format", "text", "Either json or text")
	// Quieter by default to reduce chatter in production.
	logLevel = flag.String("log-level", "warn", "Either debug, info, warn, error, fatal, panic")
	help     = flag.Bool("help", false, "Display help message")

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

	if *pollDelay < minPollDelay {
		_, _ = fmt.Fprintf(os.Stderr, "poll-delay must be >= %s\n", minPollDelay)
		os.Exit(1)
	}

	configureLogger()

	// Log version info for diagnostics and to keep ldflags-injected vars "used".
	logrus.Infof(
		"swarm-tasks-exporter starting… version=%s commit=%s date=%s",
		version,
		commit,
		date,
	)

	// Validate + set custom labels
	err := validateAndSetCustomLabels([]string(customLabels))
	if err != nil {
		logrus.Fatal(err)
	}

	// Register metrics (including health/build info)
	collector.ConfigureDesiredReplicasGauge()
	collector.ConfigureReplicasStateGauge()
	collector.ConfigureHealthGauges(version, commit, date)
	collector.ConfigureNodesByStateGauge()
	collector.ConfigureExporterOpsMetrics()

	// Root context canceled on SIGINT/SIGTERM
	rootCtx, cancel := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer cancel()

	// Docker client is configured from environment variables (DOCKER_HOST, etc.).
	cli, err := client.NewClientWithOpts(client.FromEnv)
	if err != nil {
		logrus.Fatal(err)
	}
	defer cli.Close()

	cli.NegotiateAPIVersion(rootCtx)

	// wg to wait for goroutines (event listener + poller)
	var workers sync.WaitGroup
	startEventListener(rootCtx, &workers, cli)
	startPoller(rootCtx, &workers, cli, *pollDelay)
	startHealthUpdater(rootCtx, &workers, *pollDelay)

	// HTTP server with sane timeouts + graceful shutdown.
	isHealthy := func() (bool, string) {
		return collector.HealthSnapshot(*pollDelay, time.Now())
	}
	mux := server.NewMuxWithHealth(isHealthy)

	err = runHTTPServer(rootCtx, *listenAddr, mux)
	if err != nil && !errors.Is(err, http.ErrServerClosed) && !errors.Is(err, context.Canceled) {
		logrus.Fatalf("http server error: %v", err)
	}

	// Wait for workers to exit.
	workers.Wait()
}

// --- helpers to reduce main() complexity ---

func validateAndSetCustomLabels(rawKeys []string) error {
	countErr := labelutil.ValidateCustomLabelCount(len(rawKeys))
	if countErr != nil {
		return fmt.Errorf("validate custom label count: %w", countErr)
	}

	sanitized, sanitizeErr := labelutil.ValidateAndSanitizeLabelNames(rawKeys)
	if sanitizeErr != nil {
		return fmt.Errorf("sanitize custom label names: %w", sanitizeErr)
	}

	collector.SetCustomLabels(rawKeys, sanitized)

	return nil
}

func startEventListener(ctx context.Context, wg *sync.WaitGroup, cli *client.Client) {
	wg.Add(1)

	go func() {
		defer wg.Done()

		initErr := collector.InitDesiredReplicasGauge(ctx, cli)
		if initErr != nil {
			logrus.Fatal(initErr)
		}

		listenErr := collector.ListenSwarmEvents(ctx, cli)
		if listenErr != nil && !errors.Is(listenErr, context.Canceled) {
			logrus.Error(listenErr)
		}
	}()
}

func startPoller(ctx context.Context, wg *sync.WaitGroup, cli *client.Client, delay time.Duration) {
	wg.Add(1)

	go func() {
		defer wg.Done()

		logrus.Debug("Start polling replicas state every ", delay)

		ticker := time.NewTicker(delay)
		defer ticker.Stop()

		for {
			select {
			case <-ctx.Done():
				logrus.Debug("polling loop: context canceled")

				return
			case <-ticker.C:
			}

			logrus.Debug("Polling replicas state...")

			startTime := time.Now()
			polled, pollErr := collector.PollReplicasState(ctx, cli)
			collector.ObservePollDuration(time.Since(startTime))
			collector.IncPolls()

			if pollErr != nil {
				collector.IncPollErrors()
				logrus.Error(pollErr)

				continue
			}

			collector.UpdateReplicasStateGauge(polled)
			collector.MarkPollOK(time.Now())
		}
	}()
}

func startHealthUpdater(ctx context.Context, wg *sync.WaitGroup, delay time.Duration) {
	wg.Add(1)

	go func() {
		defer wg.Done()

		ticker := time.NewTicker(healthTickInterval)
		defer ticker.Stop()

		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				healthy, _ := collector.HealthSnapshot(delay, time.Now())
				collector.SetExporterHealth(healthy)
			}
		}
	}()
}

func runHTTPServer(ctx context.Context, addr string, handler http.Handler) error {
	var srv http.Server

	srv.Addr = addr
	srv.Handler = handler
	srv.ReadHeaderTimeout = 5 * time.Second
	srv.ReadTimeout = 10 * time.Second
	srv.WriteTimeout = 15 * time.Second
	srv.IdleTimeout = 60 * time.Second

	errCh := make(chan error, 1)

	go func() {
		errCh <- srv.ListenAndServe()
	}()

	var err error

	select {
	case err = <-errCh:
		// fallthrough to shutdown path
	case <-ctx.Done():
		// context canceled: proceed to shutdown
	}

	// Graceful HTTP shutdown.
	shutdownCtx, shutdownCancel := context.WithTimeout(ctx, httpShutdownTimeout)
	defer shutdownCancel()

	shutdownErr := srv.Shutdown(shutdownCtx)
	if shutdownErr != nil {
		logrus.WithError(shutdownErr).Warn("HTTP server shutdown")
	}

	return err
}
