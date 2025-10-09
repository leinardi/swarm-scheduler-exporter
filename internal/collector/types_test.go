//nolint:testpackage // this test needs access to unexported helpers (serviceMode/buildMetadata)
package collector

import (
	"strconv"
	"testing"

	"github.com/docker/docker/api/types/swarm"
)

func TestServiceMode(t *testing.T) {
	t.Parallel()

	t.Run("replicated", func(t *testing.T) {
		t.Parallel()

		replicas := uint64(3)

		var svc swarm.Service

		svc.Spec.Mode.Replicated = &swarm.ReplicatedService{Replicas: &replicas}

		if got := serviceMode(&svc); got != "replicated" {
			t.Fatalf("got %q want %q", got, "replicated")
		}
	})

	t.Run("global", func(t *testing.T) {
		t.Parallel()

		var svc swarm.Service

		svc.Spec.Mode.Global = &swarm.GlobalService{}

		if got := serviceMode(&svc); got != "global" {
			t.Fatalf("got %q want %q", got, "global")
		}
	})
}

func TestBuildMetadata(t *testing.T) {
	t.Parallel()

	SetCustomLabels([]string{"team.label", "tier"})

	var svc swarm.Service

	idx := uint64(42)
	svc.Version.Index = idx

	// set annotations map
	svc.Spec.Annotations = swarm.Annotations{
		Name: "web",
		Labels: map[string]string{
			"com.docker.stack.namespace": "demo",
			"team.label":                 "platform",
			"tier":                       "backend",
		},
	}
	// mark it as replicated
	replicas := uint64(2)
	svc.Spec.Mode.Replicated = &swarm.ReplicatedService{Replicas: &replicas}

	metadata := buildMetadata(&svc)

	if metadata.stack != "demo" {
		t.Fatalf("stack: got %q want %q", metadata.stack, "demo")
	}

	if metadata.service != "web" {
		t.Fatalf("service: got %q want %q", metadata.service, "web")
	}

	if metadata.serviceMode != "replicated" {
		t.Fatalf("mode: got %q want %q", metadata.serviceMode, "replicated")
	}

	if metadata.serviceVersion != strconv.FormatUint(idx, 10) {
		t.Fatalf("version: got %q want %q", metadata.serviceVersion, strconv.FormatUint(idx, 10))
	}

	if metadata.customLabels["team.label"] != "platform" {
		t.Fatalf(
			"custom label team.label: got %q want %q",
			metadata.customLabels["team.label"],
			"platform",
		)
	}

	if metadata.customLabels["tier"] != "backend" {
		t.Fatalf("custom label tier: got %q want %q", metadata.customLabels["tier"], "backend")
	}
}
