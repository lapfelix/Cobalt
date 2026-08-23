#!/bin/sh
set -eu

printf '%s\n' 'Connect the Kobo Clara 2E/N506 to the same network before continuing.'
printf '%s\n' 'This script performs discovery only; the doctor itself is read-only.'
cargo run -p kobo-cli -- devices
printf '%s\n' 'Run the exact read-only probe with:'
printf '%s\n' '  cargo run -p kobo-cli -- doctor --device <reader-ip>'
