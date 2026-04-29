#!/bin/sh

set -e
set -x

rm -f gc-mavlink.log
cargo run --bin gc -- -p 4242 -f 127.0.0.1:4243

exit 0
