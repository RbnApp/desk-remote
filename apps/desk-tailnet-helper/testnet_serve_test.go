package main

import (
	"os"
	"testing"
	"time"
)

// TestServeTestnet is not a test: the Rust end-to-end test of the panel node
// (`crates/desk-transport`, `panel_node_reaches_agent_over_testcontrol`) runs it to get a local
// tailnet control server on 127.0.0.1. It writes the control URL to the file named by
// DESK_TESTNET_SERVE (nodes must sign in when DESK_TESTNET_REQUIRE_AUTH is set) and serves until that file is removed (or two minutes pass).
func TestServeTestnet(t *testing.T) {
	marker := os.Getenv("DESK_TESTNET_SERVE")
	if marker == "" {
		t.Skip("DESK_TESTNET_SERVE not set")
	}
	ctrl := startControl(t, os.Getenv("DESK_TESTNET_REQUIRE_AUTH") != "")
	if err := os.WriteFile(marker, []byte(ctrl.HTTPTestServer.URL), 0o600); err != nil {
		t.Fatalf("write control URL: %v", err)
	}
	deadline := time.Now().Add(2 * time.Minute)
	for time.Now().Before(deadline) {
		if _, err := os.Stat(marker); err != nil {
			return
		}
		time.Sleep(200 * time.Millisecond)
	}
}
