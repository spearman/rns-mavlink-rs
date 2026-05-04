#!/bin/sh

set -e
set -x

export RNS_MAVLINK_KAONIC_SETTINGS_DB_PATH="./kaonic-gateway.db"
cargo run --bin gc -- -a "192.168.10.1:9090" -l "0.0.0.0:0" -i "rns-mavlink-gc-test"

exit 0
