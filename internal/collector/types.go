package collector

import (
	"strconv"

	"github.com/docker/docker/api/types/swarm"
)

var (
	// set by main() after flags parsed.
	customLabels []string

	metadataCache = make(map[string]serviceMetadata)
)

func SetCustomLabels(ls []string) { customLabels = ls }

type serviceMetadata struct {
	stack          string
	service        string
	serviceVersion string
	serviceMode    string
	customLabels   map[string]string
}

func buildMetadata(svc swarm.Service) serviceMetadata {
	md := serviceMetadata{
		stack:          svc.Spec.Labels["com.docker.stack.namespace"],
		service:        svc.Spec.Name,
		serviceVersion: strconv.FormatUint(svc.Version.Index, 10),
		serviceMode:    serviceMode(svc),
		customLabels:   make(map[string]string),
	}

	for _, label := range customLabels {
		if val, ok := svc.Spec.Labels[label]; ok {
			md.customLabels[label] = val
		} else {
			md.customLabels[label] = ""
		}
	}

	return md
}

func serviceMode(svc swarm.Service) string {
	if svc.Spec.Mode.Replicated != nil {
		return "replicated"
	}

	return "global"
}
