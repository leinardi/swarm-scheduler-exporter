package collector

// Centralized Prometheus namespace/subsystem identifiers used across collectors.
// Keep these unexported unless an external package needs them.

const (
	// Global namespace for all metrics in this project.
	prometheusNamespace = "swarm"

	// Subsystems per functional area.
	prometheusExporterSubsystem = "exporter"
	prometheusServiceSubsystem  = "service"
	prometheusTaskSubsystem     = "task"
	prometheusClusterSubsystem  = "cluster"
)
