package main

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"tailscale.com/net/netns"
	"tailscale.com/tailcfg"
	"tailscale.com/tstest/integration"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/logger"
)

const bridgeToken = "test-token"

func TestInTailnet(t *testing.T) {
	for addr, want := range map[string]bool{
		"100.64.0.1:47800":  true,
		"100.127.1.1:1":     true,
		"100.128.0.1:1":     false,
		"127.0.0.1:47800":   false,
		"192.168.1.5:47800": false,
		"100.64.0.1":        false,
		"example.com:80":    false,
	} {
		if got := inTailnet(addr); got != want {
			t.Errorf("inTailnet(%q) = %v, want %v", addr, got, want)
		}
	}
}

// bridgeLine sends one bridge command and returns the connection plus the first reply line.
func bridgeLine(t *testing.T, bridge, command string) (net.Conn, string) {
	t.Helper()
	conn, err := net.DialTimeout("tcp", bridge, 5*time.Second)
	if err != nil {
		t.Fatalf("bridge dial: %v", err)
	}
	_ = conn.SetDeadline(time.Now().Add(20 * time.Second))
	if _, err := fmt.Fprintf(conn, "%s\n", command); err != nil {
		t.Fatalf("bridge write: %v", err)
	}
	line, _ := bufio.NewReader(conn).ReadString('\n')
	return conn, strings.TrimSpace(line)
}

func startControl(t *testing.T, requireAuth bool) *testcontrol.Server {
	t.Helper()
	netns.SetEnabled(false)
	derp := integration.RunDERPAndSTUN(t, logger.Discard, "127.0.0.1")
	ctrl := &testcontrol.Server{
		DERPMap:        derp,
		DNSConfig:      &tailcfg.DNSConfig{Proxied: true},
		MagicDNSDomain: "tail-scale.ts.net",
		Logf:           logger.Discard,
		RequireAuth:    requireAuth,
	}
	ctrl.HTTPTestServer = httptest.NewUnstartedServer(ctrl)
	ctrl.HTTPTestServer.Start()
	t.Cleanup(ctrl.HTTPTestServer.Close)
	return ctrl
}

type panelProc struct {
	cmd   *exec.Cmd
	stdin io.WriteCloser
	dec   *json.Decoder
	out   io.Reader
}

// startPanel builds and starts the real helper binary in panel mode.
func startPanel(ctx context.Context, t *testing.T, ctrl *testcontrol.Server, authKey string) *panelProc {
	t.Helper()
	exe := filepath.Join(t.TempDir(), "desk-tailnet-helper-test.exe")
	if out, err := exec.Command("go", "build", "-o", exe, ".").CombinedOutput(); err != nil {
		t.Fatalf("build helper: %v\n%s", err, out)
	}
	cmd := exec.CommandContext(ctx, exe, "panel",
		"--state-dir", filepath.Join(t.TempDir(), "panel-state"),
		"--control-url", ctrl.HTTPTestServer.URL,
		"--hostname", "deskremote-panel",
		"--bridge-addr", "127.0.0.1:0")
	cmd.Env = append(os.Environ(), "TS_AUTHKEY="+authKey, "DESK_BRIDGE_TOKEN="+bridgeToken)
	stdin, _ := cmd.StdinPipe()
	stdout, _ := cmd.StdoutPipe()
	if err := cmd.Start(); err != nil {
		t.Fatalf("start helper: %v", err)
	}
	p := &panelProc{cmd: cmd, stdin: stdin, dec: json.NewDecoder(stdout), out: stdout}
	t.Cleanup(func() { _ = stdin.Close(); _ = cmd.Wait() })
	return p
}

// next reads reports until done accepts one; it also returns the bridge address seen so far.
func (p *panelProc) next(t *testing.T, bridge *string, done func(HelperStatus) bool) HelperStatus {
	t.Helper()
	for {
		var report HelperStatus
		if err := p.dec.Decode(&report); err != nil {
			t.Fatalf("helper report: %v", err)
		}
		t.Logf("report: %+v", report)
		if report.BridgeAddr != "" {
			*bridge = report.BridgeAddr
		}
		if done(report) {
			return report
		}
	}
}

// waitExit waits until the helper closes stdout (it exited).
func (p *panelProc) waitExit(t *testing.T) {
	t.Helper()
	done := make(chan struct{})
	go func() { _, _ = io.Copy(io.Discard, p.out); close(done) }()
	select {
	case <-done:
	case <-time.After(10 * time.Second):
		t.Fatal("helper did not exit")
	}
}

// TestPanelNodeDialsAgentWithoutOfficialClient runs the real helper binary in panel mode against a
// local test control server and dials another node through its loopback bridge.
func TestPanelNodeDialsAgentWithoutOfficialClient(t *testing.T) {
	ctrl := startControl(t, false)
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	agent := createTestNode(t, ctrl.HTTPTestServer.URL, "deskremote-agent")
	defer agent.Close()
	agentStatus, err := agent.Up(ctx)
	if err != nil {
		t.Fatalf("agent up: %v", err)
	}
	ln, err := agent.Listen("tcp", ":7777")
	if err != nil {
		t.Fatalf("agent listen: %v", err)
	}
	defer ln.Close()
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func(c net.Conn) {
				defer c.Close()
				line, _ := bufio.NewReader(c).ReadString('\n')
				_, _ = fmt.Fprintf(c, "ACK %s", line)
			}(conn)
		}
	}()

	p := startPanel(ctx, t, ctrl, "tskey-test-fake")
	var bridge string
	report := p.next(t, &bridge, func(r HelperStatus) bool { return r.Status == "running" && len(r.Peers) > 0 })
	if len(report.TailscaleIPs) == 0 || report.Peers[0].HostName != "deskremote-agent" {
		t.Fatalf("unexpected running report: %+v", report)
	}

	target := fmt.Sprintf("%s:7777", agentStatus.TailscaleIPs[0])
	conn, reply := bridgeLine(t, bridge, "CONNECT "+bridgeToken+" "+target)
	if reply != "OK" {
		t.Fatalf("CONNECT reply %q", reply)
	}
	_, _ = fmt.Fprint(conn, "hello-from-panel\n")
	got, _ := bufio.NewReader(conn).ReadString('\n')
	_ = conn.Close()
	t.Logf("frame roundtrip through the panel bridge: %q", got)
	if got != "ACK hello-from-panel\n" {
		t.Fatalf("unexpected echo %q", got)
	}

	for _, bad := range []string{
		"CONNECT wrong-token " + target,
		"CONNECT " + bridgeToken + " 127.0.0.1:7777",
		"LOGOUT wrong-token",
	} {
		c, reply := bridgeLine(t, bridge, bad)
		_ = c.Close()
		if !strings.HasPrefix(reply, "ERR") {
			t.Errorf("%q must be refused, got %q", bad, reply)
		}
	}

	_ = p.stdin.Close()
	p.waitExit(t)
}

// TestPanelSignInAndLogout: without a key the helper reports the login URL, finishes once the
// login completes, and LOGOUT ends the node.
func TestPanelSignInAndLogout(t *testing.T) {
	ctrl := startControl(t, true)
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	p := startPanel(ctx, t, ctrl, "")
	var bridge string
	report := p.next(t, &bridge, func(r HelperStatus) bool { return r.Status == "needs_auth" })
	if !strings.HasPrefix(report.AuthURL, ctrl.HTTPTestServer.URL) {
		t.Fatalf("unexpected login URL %q", report.AuthURL)
	}
	if !ctrl.CompleteAuth(report.AuthURL) {
		t.Fatal("test control did not know the login URL")
	}
	p.next(t, &bridge, func(r HelperStatus) bool { return r.Status == "running" })

	conn, reply := bridgeLine(t, bridge, "LOGOUT "+bridgeToken)
	_ = conn.Close()
	if reply != "OK" {
		t.Fatalf("LOGOUT reply %q", reply)
	}
	p.waitExit(t)
}
