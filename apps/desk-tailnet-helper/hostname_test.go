package main

import (
	"context"
	"os/exec"
	"strings"
	"testing"
	"time"

	"tailscale.com/tailcfg"
)

func TestDefaultHostnameIsADNSLabel(t *testing.T) {
	got := defaultHostname()
	if got == "" || got == "deskremote-node" || len(got) > 63 || got != strings.ToLower(got) ||
		strings.HasPrefix(got, "-") || strings.HasSuffix(got, "-") || strings.ContainsAny(got, "_ .") {
		t.Fatalf("bad default host name %q", got)
	}
}

// startListenNamed starts `listen` with a given --hostname, or without the flag when it is empty.
func startListenNamed(ctx context.Context, t *testing.T, exe, controlURL, stateDir, host string) *listenProc {
	t.Helper()
	args := []string{"listen", "--state-dir", stateDir, "--control-url", controlURL,
		"--tailnet-port", "7777", "--bridge-addr", "127.0.0.1:1"}
	if host != "" {
		args = append(args, "--hostname", host)
	}
	cmd := exec.CommandContext(ctx, exe, args...)
	return attachListen(t, cmd)
}

func nodeNames(nodes []*tailcfg.Node) []string {
	var names []string
	for _, n := range nodes {
		names = append(names, n.Hostinfo.Hostname())
	}
	return names
}

// TestAgentShowsTheComputerNameAndRenamesInPlace is the DAPP-114 acceptance test: the node shows up in
// the control server under the host name it was started with (never "deskremote-node"), and when the
// same state folder is started again with another name the SAME node is renamed without a new login.
func TestAgentShowsTheComputerNameAndRenamesInPlace(t *testing.T) {
	ctrl := startControl(t, false)
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
	defer cancel()
	exe := buildHelper(t)
	state := t.TempDir()

	p := startListenNamed(ctx, t, exe, ctrl.HTTPTestServer.URL, state, "minipc")
	p.waitFor(t, 60*time.Second, func(r HelperStatus) bool { return r.Status == "listening" })
	names := nodeNames(ctrl.AllNodes())
	t.Logf("nodes after first start: %v", names)
	if len(names) != 1 || names[0] != "minipc" {
		t.Fatalf("want one node named minipc, got %v", names)
	}

	_ = p.cmd.Process.Kill()
	<-p.exited
	q := startListenNamed(ctx, t, exe, ctrl.HTTPTestServer.URL, state, "salon-pc")
	q.waitFor(t, 60*time.Second, func(r HelperStatus) bool {
		if r.Status == "needs_auth" {
			t.Fatalf("rename asked for a login again: %+v", r)
		}
		return r.Status == "listening"
	})
	names = nodeNames(ctrl.AllNodes())
	t.Logf("nodes after restart with another name: %v", names)
	if len(names) != 1 || names[0] != "salon-pc" {
		t.Fatalf("want the same node renamed to salon-pc, got %v", names)
	}
}
