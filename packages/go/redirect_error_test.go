package crawlberg

import (
	"errors"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestHTTPRedirectLoopSelectsCrawlSentinel(t *testing.T) {
	site := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, "/loop", http.StatusFound)
	}))
	defer site.Close()
	maxRedirects := uint(1)
	engine, err := CreateEngine(&CrawlConfig{
		RespectRobotsTxt: false,
		MaxRedirects:     &maxRedirects,
		Ssrf:             &SsrfPolicy{Allowlist: []HostMatcher{HostMatcherCidr{Value: "127.0.0.1/32"}}},
	})
	if err != nil {
		t.Fatalf("create engine: %v", err)
	}
	defer engine.Free()
	_, err = Scrape(engine, site.URL+"/loop")
	if !errors.Is(err, ErrCrawlTooManyRedirects) {
		t.Fatalf("HTTP redirect loop must select the crawl sentinel: %v", err)
	}
	if errors.Is(err, ErrSsrfTooManyRedirects) {
		t.Fatalf("HTTP redirect loop selected the SSRF sentinel: %v", err)
	}
	var crawlErr *CrawlError
	if !errors.As(err, &crawlErr) || crawlErr.Code != "TooManyRedirects" {
		t.Fatalf("typed error must preserve its variant: %v", err)
	}
}
