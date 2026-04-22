#!/bin/sh

set -e
set -x

cargo run --bin gc -- -a "192.168.10.1:9090" -l "0.0.0.0:0"

exit 0
