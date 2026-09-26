//go:build integration

/*
 * MIT License
 *
 * Copyright (c) 2026 Roberto Leinardi
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

package testenv

import (
	"crypto/rand"
	"encoding/hex"
	"fmt"
)

const (
	envIDHexBytes = 4 // 4 bytes → 8 hex chars

	// LabelEnvID is the Docker label key applied to every resource created by the harness. Use it
	// with LabelFilter to scope Docker API queries to one run, or on its own to find every run.
	LabelEnvID = "swarm-scheduler-exporter.it.envid"

	resourcePrefix = "sse-it"
)

// GenerateEnvID returns a random 8-character hex string for namespacing test resources.
func GenerateEnvID() (string, error) {
	buf := make([]byte, envIDHexBytes)

	_, err := rand.Read(buf)
	if err != nil {
		return "", fmt.Errorf("generate env ID: %w", err)
	}

	return hex.EncodeToString(buf), nil
}

// ResourceName returns the canonical name for a test resource: sse-it-<envID>-<base>.
func ResourceName(envID, base string) string {
	return fmt.Sprintf("%s-%s-%s", resourcePrefix, envID, base)
}

// LabelFilter returns "swarm-scheduler-exporter.it.envid=<envID>" for Docker API label filters.
func LabelFilter(envID string) string {
	return fmt.Sprintf("%s=%s", LabelEnvID, envID)
}
