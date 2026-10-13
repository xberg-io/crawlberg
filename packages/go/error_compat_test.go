package crawlberg

import (
	"errors"
	"testing"
)

func TestShouldPreserveLegacyRedirectCatchForBothTypedFamilies(t *testing.T) {
	cases := []struct {
		name     string
		sentinel error
		other    error
		legacy   bool
	}{
		{"core100", ErrCrawlTooManyRedirects, ErrSsrfTooManyRedirects, true},
		{"ssrf101", ErrSsrfTooManyRedirects, ErrCrawlTooManyRedirects, true},
		{"unrelated", ErrNotFound, ErrCrawlTooManyRedirects, false},
	}
	for _, item := range cases {
		t.Run(item.name, func(t *testing.T) {
			err := &CrawlError{sentinel: item.sentinel, Message: item.name}
			if got := errors.Is(err, ErrTooManyRedirects); got != item.legacy {
				t.Fatalf("legacy redirect catch = %v, want %v", got, item.legacy)
			}
			if !errors.Is(err, item.sentinel) || errors.Is(err, item.other) {
				t.Fatal("specific typed error families must remain distinct")
			}
		})
	}
}
