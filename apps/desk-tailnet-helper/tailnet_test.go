package main

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
	"time"

	"tailscale.com/net/netns"
	"tailscale.com/tailcfg"
	"tailscale.com/tsnet"
	"tailscale.com/tstest/integration"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/logger"
)

func createTestNode(t *testing.T, ctrlURL, name string) *tsnet.Server {
	dir := filepath.Join(t.TempDir(), name)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatalf("failed to create dir: %v", err)
	}
	return &tsnet.Server{
		Dir:        dir,
		ControlURL: ctrlURL,
		Hostname:   name,
		Ephemeral:  true,
		AuthKey:    "tskey-test-fake",
	}
}

func TestTwoNodesCommunication(t *testing.T) {
	netns.SetEnabled(false)

	derp := integration.RunDERPAndSTUN(t, logger.Discard, "127.0.0.1")
	ctrl := &testcontrol.Server{
		DERPMap:        derp,
		DNSConfig:      &tailcfg.DNSConfig{Proxied: true},
		MagicDNSDomain: "tail-scale.ts.net",
		Logf:           logger.Discard,
	}
	ctrl.HTTPTestServer = httptest.NewUnstartedServer(ctrl)
	ctrl.HTTPTestServer.Start()
	defer ctrl.HTTPTestServer.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()

	agent := createTestNode(t, ctrl.HTTPTestServer.URL, "deskremote-agent")
	defer agent.Close()

	agentStatus, err := agent.Up(ctx)
	if err != nil {
		t.Fatalf("agent up failed: %v", err)
	}
	t.Logf("agent up with ips=%v", agentStatus.TailscaleIPs)

	panel := createTestNode(t, ctrl.HTTPTestServer.URL, "deskremote-panel")
	defer panel.Close()

	if _, err := panel.Up(ctx); err != nil {
		t.Fatalf("panel up failed: %v", err)
	}
	t.Logf("panel up successfully")

	ln, err := agent.Listen("tcp", ":7777")
	if err != nil {
		t.Fatalf("agent listen failed: %v", err)
	}
	defer ln.Close()

	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func(c io.ReadWriteCloser) {
				defer c.Close()
				line, _ := bufio.NewReader(c).ReadString('\n')
				_, _ = fmt.Fprintf(c, "ACK %s", line)
			}(conn)
		}
	}()

	var dialed bool
	for attempt := 0; attempt < 30; attempt++ {
		targetAddr := fmt.Sprintf("%s:7777", agentStatus.TailscaleIPs[0])
		conn, err := panel.Dial(ctx, "tcp", targetAddr)
		if err == nil {
			t0 := time.Now()
			_, _ = fmt.Fprint(conn, "hello-desk-remote\n")
			reply, _ := bufio.NewReader(conn).ReadString('\n')
			t.Logf("roundtrip in %v reply=%q", time.Since(t0), reply)
			_ = conn.Close()
			dialed = true
			if reply != "ACK hello-desk-remote\n" {
				t.Fatalf("unexpected reply: %q", reply)
			}
			break
		}
		time.Sleep(200 * time.Millisecond)
	}

	if !dialed {
		t.Fatal("failed to dial agent over tailnet within timeout")
	}
}
