// Desk-Remote embedded tsnet helper process.
//
// Bridges the userspace Tailscale network (tsnet) with local loopback TCP sockets
// so that the native Rust agent and panel can communicate over tailnets without
// requiring root/kernel TUN drivers or external software installations.
package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"time"

	"tailscale.com/ipn"
	"tailscale.com/tsnet"
)

// HelperStatus encapsulates the JSON output for status queries.
type HelperStatus struct {
	Status       string   `json:"status"`
	AuthURL      string   `json:"auth_url,omitempty"`
	TailscaleIPs []string `json:"ips,omitempty"`
	Error        string   `json:"error,omitempty"`
}

func defaultStateDir() string {
	// Default to %ProgramData%\DeskRemote\tailscale if on Windows
	if pd := os.Getenv("ProgramData"); pd != "" {
		return filepath.Join(pd, "DeskRemote", "tailscale")
	}
	if home, err := os.UserHomeDir(); err == nil {
		return filepath.Join(home, ".deskremote", "tailscale")
	}
	return filepath.Join(os.TempDir(), "deskremote-tailscale")
}

func main() {
	if len(os.Args) < 2 {
		fmt.Println("usage: desk-tailnet-helper <auth|status|listen|proxy> [flags]")
		os.Exit(2)
	}

	subcmd := os.Args[1]
	fs := flag.NewFlagSet(subcmd, flag.ExitOnError)

	stateDir := fs.String("state-dir", defaultStateDir(), "Path to Tailscale state directory")
	controlURL := fs.String("control-url", os.Getenv("TS_CONTROL_URL"), "Custom Tailscale control plane URL")
	hostname := fs.String("hostname", "deskremote-node", "Tailnet hostname")
	bridgeAddr := fs.String("bridge-addr", "127.0.0.1:7777", "Local loopback target or bind address")
	tailnetPort := fs.Int("tailnet-port", 7777, "Tailnet listening port")
	targetAddr := fs.String("target-addr", "", "Remote tailnet peer address (e.g. 100.x.y.z:7777)")

	if err := fs.Parse(os.Args[2:]); err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}

	authKey := os.Getenv("TS_AUTHKEY")

	s := &tsnet.Server{
		Dir:        *stateDir,
		Hostname:   *hostname,
		AuthKey:    authKey,
		ControlURL: *controlURL,
	}
	defer s.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 120*time.Second)
	defer cancel()

	switch subcmd {
	case "auth":
		runAuth(ctx, s)
	case "status":
		runStatus(ctx, s)
	case "listen":
		runListen(ctx, s, *tailnetPort, *bridgeAddr)
	case "proxy":
		runProxy(ctx, s, *bridgeAddr, *targetAddr)
	default:
		outputJSON(HelperStatus{Status: "error", Error: fmt.Sprintf("unknown command: %s", subcmd)})
		os.Exit(1)
	}
}

func outputJSON(v interface{}) {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	_ = enc.Encode(v)
}

func runAuth(ctx context.Context, s *tsnet.Server) {
	if err := s.Start(); err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}

	lc, err := s.LocalClient()
	if err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}

	w, err := lc.WatchIPNBus(ctx, ipn.NotifyInitialState)
	if err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}
	defer w.Close()

	for {
		n, err := w.Next()
		if err != nil {
			outputJSON(HelperStatus{Status: "error", Error: err.Error()})
			os.Exit(1)
		}
		if n.BrowseToURL != nil {
			outputJSON(HelperStatus{
				Status:  "needs_auth",
				AuthURL: *n.BrowseToURL,
			})
			return
		}
		if n.State != nil && *n.State == ipn.Running {
			st, err := s.Up(ctx)
			var ips []string
			if err == nil {
				for _, ip := range st.TailscaleIPs {
					ips = append(ips, ip.String())
				}
			}
			outputJSON(HelperStatus{
				Status:       "running",
				TailscaleIPs: ips,
			})
			return
		}
	}
}

func runStatus(ctx context.Context, s *tsnet.Server) {
	st, err := s.Up(ctx)
	if err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}

	var ips []string
	for _, ip := range st.TailscaleIPs {
		ips = append(ips, ip.String())
	}

	outputJSON(HelperStatus{
		Status:       "running",
		TailscaleIPs: ips,
	})
}

// runListen accepts connections on the tailnet and forwards them to a local loopback port.
func runListen(ctx context.Context, s *tsnet.Server, tailnetPort int, bridgeAddr string) {
	st, err := s.Up(ctx)
	if err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}

	var ips []string
	for _, ip := range st.TailscaleIPs {
		ips = append(ips, ip.String())
	}
	outputJSON(HelperStatus{
		Status:       "listening",
		TailscaleIPs: ips,
	})

	ln, err := s.Listen("tcp", fmt.Sprintf(":%d", tailnetPort))
	if err != nil {
		os.Exit(1)
	}
	defer ln.Close()

	for {
		remoteConn, err := ln.Accept()
		if err != nil {
			return
		}

		go func(remote net.Conn) {
			defer remote.Close()
			localConn, err := net.Dial("tcp", bridgeAddr)
			if err != nil {
				return
			}
			defer localConn.Close()

			pipeBiDirectional(remote, localConn)
		}(remoteConn)
	}
}

// runProxy listens on a local loopback port and forwards connections to a tailnet destination.
func runProxy(ctx context.Context, s *tsnet.Server, listenAddr, targetAddr string) {
	if targetAddr == "" {
		outputJSON(HelperStatus{Status: "error", Error: "target-addr flag is required for proxy mode"})
		os.Exit(1)
	}

	if _, err := s.Up(ctx); err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}

	ln, err := net.Listen("tcp", listenAddr)
	if err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}
	defer ln.Close()

	outputJSON(HelperStatus{
		Status: "proxying",
	})

	for {
		localConn, err := ln.Accept()
		if err != nil {
			return
		}

		go func(local net.Conn) {
			defer local.Close()
			remoteConn, err := s.Dial(ctx, "tcp", targetAddr)
			if err != nil {
				return
			}
			defer remoteConn.Close()

			pipeBiDirectional(local, remoteConn)
		}(localConn)
	}
}

func pipeBiDirectional(c1, c2 net.Conn) {
	done := make(chan struct{}, 2)
	go func() {
		_, _ = io.Copy(c1, c2)
		done <- struct{}{}
	}()
	go func() {
		_, _ = io.Copy(c2, c1)
		done <- struct{}{}
	}()
	<-done
}
