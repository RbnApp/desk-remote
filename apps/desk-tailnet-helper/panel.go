package main

import (
	"bufio"
	"context"
	"crypto/subtle"
	"fmt"
	"io"
	"net"
	"net/netip"
	"os"
	"reflect"
	"sort"
	"strings"
	"time"

	"tailscale.com/ipn/ipnstate"
	"tailscale.com/tsnet"
)

const (
	panelPollEvery   = 2 * time.Second
	bridgeLineWait   = 5 * time.Second
	bridgeDialWait   = 10 * time.Second
	bridgeMaxLineLen = 512
)

var (
	tailnetV4 = netip.MustParsePrefix("100.64.0.0/10")
	tailnetV6 = netip.MustParsePrefix("fd7a:115c:a1e0::/48")
)

// runPanel is the panel's own tailnet node. It reports sign-in state and peers as JSON lines on
// stdout and serves a loopback bridge: a client sends `CONNECT <token> <ip:port>` and the helper
// dials that tailnet address (userspace, no official Tailscale client involved), or `LOGOUT <token>`.
// The helper exits when stdin closes, so a crashed panel never leaves it behind.
func runPanel(ctx context.Context, s *tsnet.Server, bridgeAddr, token string) {
	if token == "" {
		outputJSON(HelperStatus{Status: "error", Error: "DESK_BRIDGE_TOKEN is required for panel mode"})
		os.Exit(1)
	}
	if err := s.Start(); err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}
	lc, err := s.LocalClient()
	if err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}
	ln, err := net.Listen("tcp", bridgeAddr)
	if err != nil {
		outputJSON(HelperStatus{Status: "error", Error: err.Error()})
		os.Exit(1)
	}
	defer ln.Close()

	quit := make(chan struct{}, 1)
	go func() {
		_, _ = io.Copy(io.Discard, os.Stdin)
		quit <- struct{}{}
	}()
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go serveBridge(ctx, s, token, conn, quit)
		}
	}()

	var last HelperStatus
	tick := time.NewTicker(panelPollEvery)
	defer tick.Stop()
	for {
		report := HelperStatus{Status: "starting", BridgeAddr: ln.Addr().String()}
		if st, err := lc.Status(ctx); err == nil {
			report = panelReport(st)
			report.BridgeAddr = ln.Addr().String()
		}
		if !reflect.DeepEqual(report, last) {
			outputJSON(report)
			last = report
		}
		select {
		case <-quit:
			return
		case <-tick.C:
		}
	}
}

// panelReport maps the node status to the report the panel reads.
func panelReport(st *ipnstate.Status) HelperStatus {
	switch st.BackendState {
	case "Running":
		report := HelperStatus{Status: "running"}
		for _, ip := range st.TailscaleIPs {
			report.TailscaleIPs = append(report.TailscaleIPs, ip.String())
		}
		if st.Self != nil {
			report.HostName = st.Self.HostName
		}
		for _, peer := range st.Peer {
			info := PeerInfo{HostName: peer.HostName, Online: peer.Online}
			for _, ip := range peer.TailscaleIPs {
				info.IPs = append(info.IPs, ip.String())
			}
			report.Peers = append(report.Peers, info)
		}
		sort.Slice(report.Peers, func(i, j int) bool { return report.Peers[i].HostName < report.Peers[j].HostName })
		return report
	case "NeedsLogin", "NeedsMachineAuth":
		if st.AuthURL != "" {
			return HelperStatus{Status: "needs_auth", AuthURL: st.AuthURL}
		}
	}
	return HelperStatus{Status: "starting"}
}

// inTailnet reports whether addr (`ip:port`) is a tailnet node address; the bridge refuses anything else.
func inTailnet(addr string) bool {
	ap, err := netip.ParseAddrPort(addr)
	if err != nil {
		return false
	}
	ip := ap.Addr().Unmap()
	return tailnetV4.Contains(ip) || tailnetV6.Contains(ip)
}

func serveBridge(ctx context.Context, s *tsnet.Server, token string, conn net.Conn, quit chan<- struct{}) {
	defer conn.Close()
	_ = conn.SetReadDeadline(time.Now().Add(bridgeLineWait))
	line, err := bufio.NewReaderSize(io.LimitReader(conn, bridgeMaxLineLen), bridgeMaxLineLen).ReadString('\n')
	if err != nil {
		return
	}
	_ = conn.SetReadDeadline(time.Time{})
	fields := strings.Fields(line)
	if len(fields) < 2 || subtle.ConstantTimeCompare([]byte(fields[1]), []byte(token)) != 1 {
		_, _ = io.WriteString(conn, "ERR unauthorized\n")
		return
	}
	switch fields[0] {
	case "CONNECT":
		if len(fields) != 3 || !inTailnet(fields[2]) {
			_, _ = io.WriteString(conn, "ERR not a tailnet address\n")
			return
		}
		dialCtx, cancel := context.WithTimeout(ctx, bridgeDialWait)
		remote, err := s.Dial(dialCtx, "tcp", fields[2])
		cancel()
		if err != nil {
			_, _ = io.WriteString(conn, "ERR "+oneLine(err)+"\n")
			return
		}
		defer remote.Close()
		_, _ = io.WriteString(conn, "OK\n")
		pipeBiDirectional(conn, remote)
	case "LOGOUT":
		lc, err := s.LocalClient()
		if err == nil {
			err = lc.Logout(ctx)
		}
		if err != nil {
			_, _ = io.WriteString(conn, "ERR "+oneLine(err)+"\n")
			return
		}
		_, _ = io.WriteString(conn, "OK\n")
		quit <- struct{}{}
	default:
		_, _ = io.WriteString(conn, "ERR unknown command\n")
	}
}

func oneLine(err error) string {
	return strings.Join(strings.Fields(fmt.Sprint(err)), " ")
}
