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
	"strings"
	"sync"
	"time"

	"tailscale.com/client/local"
	"tailscale.com/ipn"
	"tailscale.com/tsnet"
)

// HelperStatus encapsulates the JSON output for status queries.
type HelperStatus struct {
	Status       string   `json:"status"`
	AuthURL      string   `json:"auth_url,omitempty"`
	TailscaleIPs []string `json:"ips,omitempty"`
	Error        string   `json:"error,omitempty"`
	// Panel mode only.
	BridgeAddr string     `json:"bridge_addr,omitempty"`
	HostName   string     `json:"host_name,omitempty"`
	Peers      []PeerInfo `json:"peers,omitempty"`
}

// PeerInfo is one other node of the tailnet (panel mode).
type PeerInfo struct {
	HostName string   `json:"host_name"`
	IPs      []string `json:"ips"`
	Online   bool     `json:"online"`
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

// defaultHostname is this computer's name as a DNS label, so a node started without --hostname is
// still listed under the PC's name (never a product placeholder).
func defaultHostname() string {
	name, _ := os.Hostname()
	var b []rune
	for _, r := range strings.ToLower(name) {
		if (r >= 'a' && r <= 'z') || (r >= '0' && r <= '9') {
			b = append(b, r)
		} else {
			b = append(b, '-')
		}
	}
	label := strings.Trim(string(b), "-")
	if len(label) > 63 {
		label = strings.Trim(label[:63], "-")
	}
	if label == "" {
		return "pc"
	}
	return label
}

func main() {
	if len(os.Args) < 2 {
		fmt.Println("usage: desk-tailnet-helper <auth|status|listen|proxy|panel> [flags]")
		os.Exit(2)
	}

	subcmd := os.Args[1]
	fs := flag.NewFlagSet(subcmd, flag.ExitOnError)

	stateDir := fs.String("state-dir", defaultStateDir(), "Path to Tailscale state directory")
	controlURL := fs.String("control-url", os.Getenv("TS_CONTROL_URL"), "Custom Tailscale control plane URL")
	hostname := fs.String("hostname", defaultHostname(), "Tailnet hostname (default: this computer's name)")
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

	// `auth`, `status` and `proxy` are short-lived commands. `panel` and `listen` are long-lived:
	// the panel node lives until its owner exits, the agent node until its supervisor stops it. A
	// listener must never give up while it waits for the one-time sign-in (DAPP-113).
	ctx, cancel := context.WithCancel(context.Background())
	if subcmd != "panel" && subcmd != "listen" {
		ctx, cancel = context.WithTimeout(context.Background(), 120*time.Second)
	}
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
	case "panel":
		runPanel(ctx, s, *bridgeAddr, os.Getenv("DESK_BRIDGE_TOKEN"))
	default:
		outputJSON(HelperStatus{Status: "error", Error: fmt.Sprintf("unknown command: %s", subcmd)})
		os.Exit(1)
	}
}

var outputMu sync.Mutex

func outputJSON(v interface{}) {
	outputMu.Lock()
	defer outputMu.Unlock()
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	_ = enc.Encode(v)
	_ = os.Stdout.Sync()
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

// loginRefresh is how often a still-unanswered sign-in gets a fresh URL, so the link the tray
// offers never goes stale while the owner is away.
const loginRefresh = 10 * time.Minute

// runListen accepts connections on the tailnet and forwards them to a local loopback port.
//
// It never gives up on its own: while the node needs the one-time sign-in it keeps reporting
// `needs_auth` with a current URL; once signed in it reports `listening` and keeps watching, so a
// later loss of the login is reported again. Any fatal error ends the process and the supervisor
// restarts it. The login is stored in the state folder, so a restart does not ask again.
func runListen(ctx context.Context, s *tsnet.Server, tailnetPort int, bridgeAddr string) {
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

	var listening sync.Once
	var needsLogin bool
	go refreshLogin(ctx, lc, &needsLogin)
	for {
		n, err := w.Next()
		if err != nil {
			outputJSON(HelperStatus{Status: "error", Error: err.Error()})
			os.Exit(1)
		}
		if n.BrowseToURL != nil {
			setNeedsLogin(true, &needsLogin)
			outputJSON(HelperStatus{Status: "needs_auth", AuthURL: *n.BrowseToURL})
		}
		if n.State == nil {
			continue
		}
		switch *n.State {
		case ipn.Running:
			setNeedsLogin(false, &needsLogin)
			listening.Do(func() { go serveTailnet(ctx, s, tailnetPort, bridgeAddr) })
			reportListening(ctx, s)
		case ipn.NeedsLogin:
			// The URL itself follows as BrowseToURL.
			setNeedsLogin(true, &needsLogin)
		}
	}
}

var loginMu sync.Mutex

func setNeedsLogin(v bool, flag *bool) {
	loginMu.Lock()
	defer loginMu.Unlock()
	*flag = v
}

// refreshLogin asks for a new sign-in URL every loginRefresh while the node is still waiting.
func refreshLogin(ctx context.Context, lc *local.Client, needsLogin *bool) {
	tick := time.NewTicker(loginRefresh)
	defer tick.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-tick.C:
			loginMu.Lock()
			waiting := *needsLogin
			loginMu.Unlock()
			if waiting {
				_ = lc.StartLoginInteractive(ctx)
			}
		}
	}
}

func reportListening(ctx context.Context, s *tsnet.Server) {
	st, err := s.Up(ctx)
	if err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}
	var ips []string
	for _, ip := range st.TailscaleIPs {
		ips = append(ips, ip.String())
	}
	outputJSON(HelperStatus{Status: "listening", TailscaleIPs: ips})
}

// serveTailnet forwards tailnet connections on tailnetPort to the local bridge address.
func serveTailnet(ctx context.Context, s *tsnet.Server, tailnetPort int, bridgeAddr string) {
	ln, err := s.Listen("tcp", fmt.Sprintf(":%d", tailnetPort))
	if err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
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
