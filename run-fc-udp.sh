#!/bin/sh

set -e
set -x

rm -f fc-mavlink.log
cargo run --bin fc -- -p 4243 -f 127.0.0.1:4242 -i "rns-mavlink-fc-test"

exit 0
