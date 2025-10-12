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
	"github.com/leinardi/swarm-scheduler-exporter/internal/collector"
	labelutil "github.com/leinardi/swarm-scheduler-exporter/internal/labels"
	"github.com/leinardi/swarm-scheduler-exporter/internal/logger"
	"github.com/leinardi/swarm-scheduler-exporter/internal/server"
)

const (
	// DefaultPollDelay is the default interval between polls.
	DefaultPollDelay = 10 * time.Second

	// Operability constants.
	minPollDelay        = 1 * time.Second
	httpShutdownTimeout = 10 * time.Second
	healthTickInterval  = 5 * time.Second
)

// stringSlice implements flag.Value to support repeated -label flags
// (e.g., -label team -label tier). Each call to Set appends a value.
type stringSlice []string

// ErrEmptyFlagValue is returned when a repeated flag (like -label)
// is provided with an empty value. This is a sentinel error for tests
// and for clearer calling code.
var ErrEmptyFlagValue = errors.New("empty flag value")

// String returns the flag value in a human-friendly form.
func (values *stringSlice) String() string {
	return fmt.Sprint(*values)
}

// Set implements flag.Value for stringSlice by appending non-empty values.
func (values *stringSlice) Set(value string) error {
	if value == "" {
		return ErrEmptyFlagValue
	}

	*values = append(*values, value)

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
	logFormat = flag.String("log-format", "plain", "Either json, text or plain")
	// Quieter by default to reduce chatter in production.
	logLevel = flag.String("log-level", "info", "Either debug, info, warn, error, fatal, panic")
	logTime  = flag.Bool("log-time", false, "Include timestamp in logs")
	help     = flag.Bool("help", false, "Display help message")

	customLabels stringSlice
)

// usage prints flag usage to stdout. We avoid fmt.Print* linters by
// writing to an explicit writer and by setting the flag package's output.
func usage() {
	outWriter := os.Stdout
	_, _ = fmt.Fprintf(outWriter, "Usage of %s:\n", os.Args[0])
	flag.CommandLine.SetOutput(outWriter)
	flag.PrintDefaults()
}

// main initializes logging, Prometheus collectors, the Docker client,
// starts the polling and events goroutines, and serves /metrics with timeouts.
func main() {
	os.Exit(run())
}

// run contains the full program logic and returns an exit code.
// Defers inside run() (e.g., cancel(), Close(), etc.) will execute.
func run() int {
	flag.Var(&customLabels, "label", "Name of custom service labels to add to metrics")
	flag.Parse()

	if *help {
		usage()

		return 0
	}

	if *pollDelay < minPollDelay {
		_, _ = fmt.Fprintf(os.Stderr, "poll-delay must be >= %s\n", minPollDelay)

		return 1
	}

	// Configure slog logger according to flags.
	_ = logger.Configure(*logFormat, *logLevel, *logTime)
	loggerInstance := logger.L()

	// Log version info for diagnostics and to keep ldflags-injected vars "used".
	loggerInstance.Info("swarm-scheduler-exporter starting",
		"version", version,
		"commit", commit,
		"date", date,
	)

	// Validate + set custom labels
	validateErr := validateAndSetCustomLabels([]string(customLabels))
	if validateErr != nil {
		loggerInstance.Error("invalid custom labels", "err", validateErr)

		return 1
	}

	// Register metrics (including health/build info)
	collector.ConfigureDesiredReplicasGauge()
	collector.ConfigureReplicasStateGauge()
	collector.ConfigureHealthGauges(version, commit, date)
	collector.ConfigureNodesByStateGauge()
	collector.ConfigureExporterOpsMetrics()
	collector.ConfigureServiceUpdateMetrics()

	// Root context canceled on SIGINT/SIGTERM
	rootContext, cancelRoot := signal.NotifyContext(
		context.Background(),
		syscall.SIGINT,
		syscall.SIGTERM,
	)
	defer cancelRoot()

	// Docker client is configured from environment variables (DOCKER_HOST, etc.).
	dockerClient, newClientErr := client.NewClientWithOpts(client.FromEnv)
	if newClientErr != nil {
		loggerInstance.Error("docker client init failed", "err", newClientErr)

		return 1
	}
	defer dockerClient.Close()

	dockerClient.NegotiateAPIVersion(rootContext)

	// WaitGroup to wait for goroutines (event listener + poller + health updater).
	var workerGroup sync.WaitGroup
	startEventListener(rootContext, &workerGroup, dockerClient)
	startPoller(rootContext, &workerGroup, dockerClient, *pollDelay)
	startHealthUpdater(rootContext, &workerGroup, *pollDelay)

	// HTTP server with sane timeouts + graceful shutdown.
	isHealthy := func() (bool, string) {
		return collector.HealthSnapshot(*pollDelay, time.Now())
	}
	httpMux := server.NewMuxWithHealth(isHealthy)

	runError := runHTTPServer(rootContext, *listenAddr, httpMux)
	if runError != nil && !errors.Is(runError, http.ErrServerClosed) &&
		!errors.Is(runError, context.Canceled) {
		loggerInstance.Error("http server error", "err", runError)
	}

	// Wait for workers to exit.
	workerGroup.Wait()

	return 0
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

func startEventListener(
	parentContext context.Context,
	waitGroup *sync.WaitGroup,
	dockerClient *client.Client,
) {
	waitGroup.Add(1)

	go func() {
		defer waitGroup.Done()

		loggerInstance := logger.L()

		// Init now returns an anchor to use as the initial "since" for events.
		initialSinceAnchor, initErr := collector.InitDesiredReplicasGauge(
			parentContext,
			dockerClient,
		)
		if initErr != nil {
			loggerInstance.Error("InitDesiredReplicasGauge failed", "err", initErr)
			// If this fails, there is no point continuing.
			return
		}

		listenErr := collector.ListenSwarmEvents(parentContext, dockerClient, initialSinceAnchor)
		if listenErr != nil && !errors.Is(listenErr, context.Canceled) {
			loggerInstance.Error("event listener exited with error", "err", listenErr)
		}
	}()
}

func startPoller(
	parentContext context.Context,
	waitGroup *sync.WaitGroup,
	dockerClient *client.Client,
	delay time.Duration,
) {
	waitGroup.Add(1)

	go func() {
		defer waitGroup.Done()

		loggerInstance := logger.L()
		loggerInstance.Debug("start polling replicas state", "every", delay)

		ticker := time.NewTicker(delay)
		defer ticker.Stop()

		// Local helper to run one full poll cycle with metrics + health.
		pollOnce := func(now time.Time) {
			loggerInstance.Debug("polling replicas state")

			startTime := now
			polledStates, pollErr := collector.PollReplicasState(parentContext, dockerClient)
			collector.ObservePollDuration(time.Since(startTime))
			collector.IncPolls()

			if pollErr != nil {
				collector.IncPollErrors()
				loggerInstance.Error("poll replicas state failed", "err", pollErr)

				return
			}

			collector.UpdateReplicasStateGauge(polledStates)
			collector.MarkPollOK(now)

			healthy, _ := collector.HealthSnapshot(delay, now)
			collector.SetExporterHealth(healthy)
		}

		// --- Immediate first poll (no waiting for the first tick) ---
		pollOnce(time.Now())

		for {
			select {
			case <-parentContext.Done():
				loggerInstance.Debug("polling loop: context canceled")

				return
			case <-ticker.C:
				pollOnce(time.Now())
			}
		}
	}()
}

func startHealthUpdater(
	parentContext context.Context,
	waitGroup *sync.WaitGroup,
	delay time.Duration,
) {
	waitGroup.Add(1)

	go func() {
		defer waitGroup.Done()

		ticker := time.NewTicker(healthTickInterval)
		defer ticker.Stop()

		for {
			select {
			case <-parentContext.Done():
				return
			case <-ticker.C:
				healthy, _ := collector.HealthSnapshot(delay, time.Now())
				collector.SetExporterHealth(healthy)
			}
		}
	}()
}

func runHTTPServer(parentContext context.Context, address string, handler http.Handler) error {
	httpServer := &http.Server{
		Addr:              address,
		Handler:           handler,
		ReadHeaderTimeout: 5 * time.Second,
		ReadTimeout:       10 * time.Second,
		WriteTimeout:      15 * time.Second,
		IdleTimeout:       60 * time.Second,
	}

	errorChannel := make(chan error, 1)

	go func() {
		errorChannel <- httpServer.ListenAndServe()
	}()

	var resultError error

	select {
	case resultError = <-errorChannel:
		// fallthrough to shutdown path
	case <-parentContext.Done():
		// context canceled: proceed to shutdown
	}

	// Graceful HTTP shutdown.
	shutdownContext, shutdownCancel := context.WithTimeout(parentContext, httpShutdownTimeout)
	defer shutdownCancel()

	shutdownErr := httpServer.Shutdown(shutdownContext)
	if shutdownErr != nil {
		logger.L().Warn("HTTP server shutdown", "err", shutdownErr)
	}

	return resultError
}
