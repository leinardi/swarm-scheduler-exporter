/*
 * MIT License
 *
 * Copyright (c) 2025 Roberto Leinardi
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

package main

import (
	"crypto/tls"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"time"

	"github.com/docker/go-connections/tlsconfig"
	"github.com/moby/moby/client"
)

const (
	// dockerTLSHandshakeTimeout and dockerResponseHeaderTimeout bound the setup of every Docker
	// connection at transport level; dialing is already bounded by the client's own dialer. The
	// event stream carries no request deadline, since it must stay open, so without these a
	// daemon that accepts the connection and never answers would hold the listener forever
	// instead of making it reconnect. The daemon sends the stream's headers as soon as it opens.
	dockerTLSHandshakeTimeout   = 10 * time.Second
	dockerResponseHeaderTimeout = 30 * time.Second
)

// newDockerClient builds the Docker client from the environment, like client.New(client.FromEnv),
// on a transport whose connection setup is bounded (see dockerResponseHeaderTimeout).
//
// client.FromEnv cannot be combined with a custom transport: when DOCKER_CERT_PATH is set it
// replaces the HTTP client and its transport, and client.WithTimeout is a total request timeout
// that would cut the event stream. So this applies FromEnv's three parts itself, in its order,
// onto a transport it owns: the TLS configuration from DOCKER_CERT_PATH and DOCKER_TLS_VERIFY
// (built as client.WithTLSClientConfigFromEnv builds it), then the host, whose dialer
// client.WithHost installs exactly as FromEnv's WithHostFromEnv does, then the API version.
func newDockerClient() (*client.Client, error) {
	return newDockerClientWithTimeouts(dockerTLSHandshakeTimeout, dockerResponseHeaderTimeout)
}

func newDockerClientWithTimeouts(
	tlsHandshakeTimeout, responseHeaderTimeout time.Duration,
) (*client.Client, error) {
	tlsConfig, tlsErr := tlsConfigFromEnv()
	if tlsErr != nil {
		return nil, tlsErr
	}

	transport := &http.Transport{
		TLSClientConfig:       tlsConfig,
		TLSHandshakeTimeout:   tlsHandshakeTimeout,
		ResponseHeaderTimeout: responseHeaderTimeout,
		// A non-nil TLSNextProto keeps HTTP/2 off, as on the client's default transport. Without
		// it the clone client.WithHTTPClient makes would enable HTTP/2, which gives the transport
		// a TLS configuration, and the client would then speak HTTPS to a plain socket.
		TLSNextProto: map[string]func(string, *tls.Conn) http.RoundTripper{},
	}

	dockerClient, newErr := client.New(
		client.WithHTTPClient(
			&http.Client{Transport: transport, CheckRedirect: client.CheckRedirect},
		),
		// Installs the dialer for the default socket; WithHostFromEnv replaces it for DOCKER_HOST.
		client.WithHost(client.DefaultDockerHost),
		client.WithHostFromEnv(),
		client.WithAPIVersionFromEnv(),
	)
	if newErr != nil {
		return nil, fmt.Errorf("docker client: %w", newErr)
	}

	return dockerClient, nil
}

// tlsConfigFromEnv returns the TLS configuration client.WithTLSClientConfigFromEnv would use, or
// nil when DOCKER_CERT_PATH is unset or empty.
func tlsConfigFromEnv() (*tls.Config, error) {
	certPath := os.Getenv(client.EnvOverrideCertPath)
	if certPath == "" {
		return nil, nil //nolint:nilnil // no TLS configured is a valid result, not an error
	}

	tlsConfig, tlsErr := tlsconfig.Client(tlsconfig.Options{
		CAFile:             filepath.Join(certPath, "ca.pem"),
		CertFile:           filepath.Join(certPath, "cert.pem"),
		KeyFile:            filepath.Join(certPath, "key.pem"),
		InsecureSkipVerify: os.Getenv(client.EnvTLSVerify) == "",
		MinVersion:         tls.VersionTLS12,
	})
	if tlsErr != nil {
		return nil, fmt.Errorf(
			"configure TLS from %s=%s: %w",
			client.EnvOverrideCertPath,
			certPath,
			tlsErr,
		)
	}

	return tlsConfig, nil
}
