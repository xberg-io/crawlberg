package crawlberg

import "errors"

// ErrTooManyRedirects preserves redirect catches across the core and SSRF error families. ~keep
//
// Deprecated: use ErrCrawlTooManyRedirects or ErrSsrfTooManyRedirects for a specific family. ~keep
var ErrTooManyRedirects = errors.New("too many redirects")

// Is preserves the shared redirect sentinel while Unwrap retains each typed family. ~keep
func (e *CrawlError) Is(target error) bool {
	return target == ErrTooManyRedirects &&
		(e.sentinel == ErrCrawlTooManyRedirects || e.sentinel == ErrSsrfTooManyRedirects)
}
