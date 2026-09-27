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
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/pem"
	"errors"
	"math/big"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/moby/moby/client"
)

// clearDockerEnv unsets every variable the client reads, for the duration of the test.
func clearDockerEnv(t *testing.T) {
	t.Helper()

	for _, name := range []string{
		client.EnvOverrideHost,
		client.EnvOverrideAPIVersion,
		client.EnvOverrideCertPath,
		client.EnvTLSVerify,
	} {
		t.Setenv(name, "")
		_ = os.Unsetenv(name)
	}
}

// swarmAPIHandler answers the ping and a node list, which is all these tests call.
func swarmAPIHandler() http.Handler {
	return http.HandlerFunc(func(responseWriter http.ResponseWriter, request *http.Request) {
		responseWriter.Header().Set("Api-Version", client.MaxAPIVersion)
		responseWriter.Header().Set("Content-Type", "application/json")

		if request.URL.Path == "/_ping" {
			_, _ = responseWriter.Write([]byte("OK"))

			return
		}

		_, _ = responseWriter.Write([]byte("[]"))
	})
}

// Not parallel: t.Setenv changes the process environment the client reads.
func TestNewDockerClient_DefaultHost(t *testing.T) {
	clearDockerEnv(t)

	dockerClient, newErr := newDockerClient()
	if newErr != nil {
		t.Fatalf("newDockerClient: %v", newErr)
	}

	t.Cleanup(func() { _ = dockerClient.Close() })

	if got := dockerClient.DaemonHost(); got != client.DefaultDockerHost {
		t.Errorf("daemon host = %q, want %q", got, client.DefaultDockerHost)
	}
}

func TestNewDockerClient_UnixSocketFromEnv(t *testing.T) {
	clearDockerEnv(t)

	socketPath := filepath.Join(t.TempDir(), "docker.sock")

	listener, listenErr := new(net.ListenConfig).Listen(t.Context(), "unix", socketPath)
	if listenErr != nil {
		t.Fatalf("listen: %v", listenErr)
	}

	server := &httptest.Server{
		Listener: listener,
		Config:   &http.Server{Handler: swarmAPIHandler(), ReadHeaderTimeout: time.Second},
	}
	server.Start()
	t.Cleanup(server.Close)

	t.Setenv(client.EnvOverrideHost, "unix://"+socketPath)

	dockerClient, newErr := newDockerClient()
	if newErr != nil {
		t.Fatalf("newDockerClient: %v", newErr)
	}

	t.Cleanup(func() { _ = dockerClient.Close() })

	_, listErr := dockerClient.NodeList(t.Context(), client.NodeListOptions{})
	if listErr != nil {
		t.Errorf("node list over the unix socket: %v", listErr)
	}
}

func TestNewDockerClient_ResponseHeaderTimeoutBoundsTheEventStream(t *testing.T) {
	clearDockerEnv(t)

	// Accepts connections and never answers them.
	listener, listenErr := new(net.ListenConfig).Listen(t.Context(), "tcp", "127.0.0.1:0")
	if listenErr != nil {
		t.Fatalf("listen: %v", listenErr)
	}

	t.Cleanup(func() { _ = listener.Close() })

	go func() {
		for {
			conn, acceptErr := listener.Accept()
			if acceptErr != nil {
				return
			}

			t.Cleanup(func() { _ = conn.Close() })
		}
	}()

	t.Setenv(client.EnvOverrideHost, "tcp://"+listener.Addr().String())
	// A pinned version skips the negotiation ping, so the stream request is the first one sent.
	t.Setenv(client.EnvOverrideAPIVersion, client.MaxAPIVersion)

	const responseHeaderTimeout = 100 * time.Millisecond

	dockerClient, newErr := newDockerClientWithTimeouts(time.Second, responseHeaderTimeout)
	if newErr != nil {
		t.Fatalf("newDockerClient: %v", newErr)
	}

	t.Cleanup(func() { _ = dockerClient.Close() })

	// No deadline on the stream's context, as the listener opens it.
	eventsResult := dockerClient.Events(context.Background(), client.EventsListOptions{})

	select {
	case streamErr := <-eventsResult.Err:
		if streamErr == nil {
			t.Error("the stream ended without an error")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("the stream to a silent daemon was still waiting for headers after 5s")
	}
}

func TestNewDockerClient_TLSFromEnv(t *testing.T) {
	clearDockerEnv(t)

	certDir := t.TempDir()
	serverTLS := writeTestTLSMaterial(t, certDir)

	server := httptest.NewUnstartedServer(swarmAPIHandler())
	server.TLS = serverTLS
	server.StartTLS()
	t.Cleanup(server.Close)

	t.Setenv(client.EnvOverrideHost, "tcp://"+server.Listener.Addr().String())
	t.Setenv(client.EnvOverrideCertPath, certDir)
	t.Setenv(client.EnvTLSVerify, "1")

	dockerClient, newErr := newDockerClient()
	if newErr != nil {
		t.Fatalf("newDockerClient: %v", newErr)
	}

	t.Cleanup(func() { _ = dockerClient.Close() })

	// The server requires a client certificate signed by the CA, and the client verifies the
	// server against ca.pem: the call only succeeds with both halves of the TLS configuration.
	_, listErr := dockerClient.NodeList(t.Context(), client.NodeListOptions{})
	if listErr != nil {
		t.Errorf("node list over mutual TLS: %v", listErr)
	}
}

func TestNewDockerClient_TLSFromEnvMissingFiles(t *testing.T) {
	clearDockerEnv(t)
	t.Setenv(client.EnvOverrideCertPath, t.TempDir())

	_, newErr := newDockerClient()
	if newErr == nil {
		t.Error("newDockerClient succeeded with an empty DOCKER_CERT_PATH directory")
	}

	if _, ok := errors.AsType[*os.PathError](newErr); !ok {
		t.Errorf("err = %v, want the missing file reported", newErr)
	}
}

// writeTestTLSMaterial writes ca.pem, cert.pem and key.pem (a client certificate) to dir, and
// returns a server TLS configuration for 127.0.0.1 that requires a client certificate from the
// same CA.
func writeTestTLSMaterial(t *testing.T, dir string) *tls.Config {
	t.Helper()

	caKey := newTestKey(t)
	caTemplate := &x509.Certificate{
		SerialNumber:          big.NewInt(1),
		Subject:               pkix.Name{CommonName: "test CA"},
		NotBefore:             time.Now().Add(-time.Hour),
		NotAfter:              time.Now().Add(time.Hour),
		IsCA:                  true,
		KeyUsage:              x509.KeyUsageCertSign,
		BasicConstraintsValid: true,
	}
	caDER := createTestCert(t, caTemplate, caTemplate, caKey, caKey)

	caCert, parseErr := x509.ParseCertificate(caDER)
	if parseErr != nil {
		t.Fatalf("parse CA: %v", parseErr)
	}

	clientKey := newTestKey(t)
	clientDER := createTestCert(t, &x509.Certificate{
		SerialNumber: big.NewInt(2),
		Subject:      pkix.Name{CommonName: "test client"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageClientAuth},
	}, caCert, clientKey, caKey)

	serverKey := newTestKey(t)
	serverDER := createTestCert(t, &x509.Certificate{
		SerialNumber: big.NewInt(3),
		Subject:      pkix.Name{CommonName: "127.0.0.1"},
		NotBefore:    time.Now().Add(-time.Hour),
		NotAfter:     time.Now().Add(time.Hour),
		KeyUsage:     x509.KeyUsageDigitalSignature,
		ExtKeyUsage:  []x509.ExtKeyUsage{x509.ExtKeyUsageServerAuth},
		IPAddresses:  []net.IP{net.ParseIP("127.0.0.1")},
	}, caCert, serverKey, caKey)

	writePEM(t, filepath.Join(dir, "ca.pem"), "CERTIFICATE", caDER)
	writePEM(t, filepath.Join(dir, "cert.pem"), "CERTIFICATE", clientDER)
	writePEM(t, filepath.Join(dir, "key.pem"), "EC PRIVATE KEY", marshalTestKey(t, clientKey))

	clientCAs := x509.NewCertPool()
	clientCAs.AddCert(caCert)

	return &tls.Config{
		Certificates: []tls.Certificate{{Certificate: [][]byte{serverDER}, PrivateKey: serverKey}},
		ClientAuth:   tls.RequireAndVerifyClientCert,
		ClientCAs:    clientCAs,
		MinVersion:   tls.VersionTLS12,
	}
}

func newTestKey(t *testing.T) *ecdsa.PrivateKey {
	t.Helper()

	key, keyErr := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if keyErr != nil {
		t.Fatalf("generate key: %v", keyErr)
	}

	return key
}

func marshalTestKey(t *testing.T, key *ecdsa.PrivateKey) []byte {
	t.Helper()

	der, marshalErr := x509.MarshalECPrivateKey(key)
	if marshalErr != nil {
		t.Fatalf("marshal key: %v", marshalErr)
	}

	return der
}

func createTestCert(
	t *testing.T,
	template, parent *x509.Certificate,
	key, parentKey *ecdsa.PrivateKey,
) []byte {
	t.Helper()

	der, createErr := x509.CreateCertificate(
		rand.Reader,
		template,
		parent,
		&key.PublicKey,
		parentKey,
	)
	if createErr != nil {
		t.Fatalf("create certificate: %v", createErr)
	}

	return der
}

func writePEM(t *testing.T, path, blockType string, der []byte) {
	t.Helper()

	writeErr := os.WriteFile(
		path,
		pem.EncodeToMemory(&pem.Block{Type: blockType, Bytes: der}),
		0o600,
	)
	if writeErr != nil {
		t.Fatalf("write %s: %v", path, writeErr)
	}
}
