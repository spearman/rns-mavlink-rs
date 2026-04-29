#!/bin/sh

set -e
set -x

rm -f fc-mavlink.log
cargo run --bin fc -- -p 4243 -f 127.0.0.1:4242

exit 0
