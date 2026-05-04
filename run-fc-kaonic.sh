#!/bin/sh

set -e
set -x

#stty -F /dev/ttySTM1 57600

cargo run --bin fc -- -a "192.168.10.1:9090" -l "0.0.0.0:0" -i "rns-mavlink-fc-test"

exit 0
