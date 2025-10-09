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
	"github.com/leinardi/swarm-tasks-exporter/internal/server"
	"github.com/sirupsen/logrus"
)

const (
	// DefaultPollDelay is the default interval between polls.
	DefaultPollDelay = 10 * time.Second
)

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

type stringSlice []string

var ErrEmptyFlagValue = errors.New("empty flag value")

func (i *stringSlice) String() string {
	return fmt.Sprint(*i)
}

func (i *stringSlice) Set(value string) error {
	if value == "" {
		return ErrEmptyFlagValue
	}

	*i = append(*i, value)

	return nil
}

var (
	listenAddr = flag.String("listen-addr", "0.0.0.0:8888", "IP address and port to bind")
	pollDelay  = flag.Duration("poll-delay", DefaultPollDelay, "Delay in seconds between two polls")
	logFormat  = flag.String("log-format", "text", "Either json or text")
	logLevel   = flag.String("log-level", "info", "Either debug, info, warn, error, fatal, panic")
	help       = flag.Bool("help", false, "Display help message")

	customLabels stringSlice
)

func usage() {
	// Avoid forbidigo: write to explicit writer instead of Printf
	w := os.Stdout
	_, _ = fmt.Fprintf(w, "Usage of %s:\n", os.Args[0])
	flag.CommandLine.SetOutput(w)
	flag.PrintDefaults()
}

func main() {
	flag.Var(&customLabels, "label", "Name of custom service labels to add to metrics")
	flag.Parse()

	if *help {
		usage()
		os.Exit(0)
	}

	configureLogger()

	// log version to avoid 'unused' for version vars and for observability
	logrus.Infof(
		"swarm-tasks-exporter starting… version=%s commit=%s date=%s",
		version,
		commit,
		date,
	)

	collector.SetCustomLabels([]string(customLabels))
	collector.ConfigureDesiredReplicasGauge()
	collector.ConfigureReplicasStateGauge()

	ctx := context.Background()

	cli, err := client.NewClientWithOpts(client.FromEnv)
	if err != nil {
		logrus.Fatal(err)
	}
	defer cli.Close()

	cli.NegotiateAPIVersion(ctx)

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
