# crawlberg-cli

CLI proxy for [`crawlberg`](https://github.com/xberg-io/crawlberg). Installing
this package provides a `crawlberg` command. Platform wheels bundle the native
binary; the source distribution downloads the binary of the matching release
from GitHub and verifies it against that release's SHA256SUMS.

The package version is the crawlberg release version, so a given version of this
package only ever runs (or downloads) that release. Override the release tag
explicitly with the `CRAWLBERG_CLI_VERSION` environment variable.
