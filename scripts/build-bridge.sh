#!/bin/sh
set -eu
if [ "$(uname -s)" != Darwin ]; then
  printf '%s\n' 'Apple Foundation Models requires macOS and Xcode 26 or newer.' >&2
  exit 1
fi
# xcode-select -p never opens the "install developer tools" dialog; xcrun can.
if ! developer_dir=$(/usr/bin/xcode-select -p 2>/dev/null) || [ ! -d "$developer_dir" ]; then
  printf '%s\n' "Apple's command line tools aren't installed. Nothing was installed." \
    'Install them with: xcode-select --install' >&2
  exit 1
fi
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
output=${1:-"$root/target/debug/apple-bridge"}
mkdir -p "$(dirname -- "$output")"
/usr/bin/xcrun --sdk macosx swiftc -parse-as-library -O -target arm64-apple-macosx26.0 \
  "$root/native/AppleBridge.swift" -o "$output"
printf '%s\n' "$output"
