# shellcheck shell=bash
# Starts the e2e mock server for the calling file task, stops it when the task exits, and exports
# MOCK_SERVER_URL and MOCK_SERVERS from the .mock-server.env it writes.
start_mock_server() {
  mise run mock-server:start
  trap 'mise run mock-server:stop' EXIT
  # shellcheck disable=SC1091 # written by mock-server:start
  . ./.mock-server.env
  export MOCK_SERVER_URL MOCK_SERVERS
}
