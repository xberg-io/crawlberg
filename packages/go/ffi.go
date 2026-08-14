//go:build !windows

package crawlberg

/*
// Crawlberg FFI - CGO Configuration (non-Windows)
//
// This file provides the CGO include directive for the FFI header.
// Library linking is configured via:
//   - CI: CGO_CFLAGS and CGO_LDFLAGS environment variables set by setup-go-cgo-env action
//   - Development: Use -tags crawlberg_dev for monorepo builds with hardcoded paths
//   - Production: Run go generate to download FFI and generate cgo_flags.go
//
// The CFLAGS directive below provides the include path for the header file.
// LDFLAGS must be provided externally (via env vars or cgo_flags.go).

#cgo CFLAGS: -I${SRCDIR}/include

#include "crawlberg.h"
#include <stdlib.h>
#include <stdint.h>
*/
import "C"
