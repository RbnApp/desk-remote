package main

import (
	"context"
	"encoding/json"
	"io"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// listenProc is the real helper binary running in agent (`listen`) mode.
type listenProc struct {
	cmd     *exec.Cmd
	reports chan HelperStatus
	exited  chan struct{}
}

func buildHelper(t *testing.T) string {
	t.Helper()
	exe := filepath.Join(t.TempDir(), "desk-tailnet-helper-test.exe")
	if out, err := exec.Command("go", "build", "-o", exe, ".").CombinedOutput(); err != nil {
		t.Fatalf("build helper: %v\n%s", err, out)
	}
	return exe
}

// startListen starts `listen` WITHOUT an auth key against the control server and the state folder.
func startListen(ctx context.Context, t *testing.T, exe, controlURL, stateDir string) *listenProc {
	t.Helper()
	cmd := exec.CommandContext(ctx, exe, "listen",
		"--state-dir", stateDir,
		"--control-url", controlURL,
		"--hostname", "deskremote-agent",
		"--tailnet-port", "7777",
		"--bridge-addr", "127.0.0.1:1")
	return attachListen(t, cmd)
}

// attachListen starts cmd and reads its JSON reports.
func attachListen(t *testing.T, cmd *exec.Cmd) *listenProc {
	t.Helper()
	stdout, _ := cmd.StdoutPipe()
	cmd.Stdin = nil
	if err := cmd.Start(); err != nil {
		t.Fatalf("start helper: %v", err)
	}
	p := &listenProc{cmd: cmd, reports: make(chan HelperStatus, 64), exited: make(chan struct{})}
	go func() {
		dec := json.NewDecoder(stdout)
		for {
			var r HelperStatus
			if err := dec.Decode(&r); err != nil {
				if err != io.EOF {
					t.Logf("report stream ended: %v", err)
				}
				close(p.exited)
				return
			}
			p.reports <- r
		}
	}()
	t.Cleanup(func() { _ = cmd.Process.Kill(); _ = cmd.Wait() })
	return p
}

func (p *listenProc) alive() bool {
	select {
	case <-p.exited:
		return false
	default:
		return true
	}
}

// waitFor returns the first report accepted by done; it fails if the helper exits or time runs out.
func (p *listenProc) waitFor(t *testing.T, within time.Duration, done func(HelperStatus) bool) HelperStatus {
	t.Helper()
	deadline := time.After(within)
	for {
		select {
		case r := <-p.reports:
			t.Logf("report: %+v", r)
			if done(r) {
				return r
			}
		case <-p.exited:
			t.Fatal("helper exited")
		case <-deadline:
			t.Fatal("no matching report in time")
		}
	}
}

// TestListenSurvivesSignInAndRestartsWithoutAsking is the DAPP-113 acceptance test: with no key the
// agent helper reports the login URL and is STILL alive after the old 120 s limit; completing the
// login makes it listen; after it is killed, a restart on the same state folder listens again
// without asking for a login.
func TestListenSurvivesSignInAndRestartsWithoutAsking(t *testing.T) {
	if testing.Short() {
		t.Skip("takes over two minutes")
	}
	ctrl := startControl(t, true)
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	exe := buildHelper(t)
	state := t.TempDir()

	p := startListen(ctx, t, exe, ctrl.HTTPTestServer.URL, state)
	login := p.waitFor(t, 60*time.Second, func(r HelperStatus) bool { return r.Status == "needs_auth" })
	if !strings.HasPrefix(login.AuthURL, ctrl.HTTPTestServer.URL) {
		t.Fatalf("unexpected login URL %q", login.AuthURL)
	}

	// The old helper gave up after 120 s. This one must still be waiting.
	time.Sleep(125 * time.Second)
	if !p.alive() {
		t.Fatal("helper died while waiting for the sign-in")
	}

	if !ctrl.CompleteAuth(login.AuthURL) {
		t.Fatal("test control did not know the login URL")
	}
	up := p.waitFor(t, 60*time.Second, func(r HelperStatus) bool { return r.Status == "listening" })
	if len(up.TailscaleIPs) == 0 {
		t.Fatalf("listening without an address: %+v", up)
	}

	// Crash: kill the helper, then start it again on the same folder.
	_ = p.cmd.Process.Kill()
	<-p.exited
	q := startListen(ctx, t, exe, ctrl.HTTPTestServer.URL, state)
	q.waitFor(t, 60*time.Second, func(r HelperStatus) bool {
		if r.Status == "needs_auth" {
			t.Fatalf("restart asked for a login again: %+v", r)
		}
		return r.Status == "listening"
	})
}
