#!/bin/sh

set -e
set -x

stty -F /dev/ttySTM1 57600

cargo run --bin fc -- -a http://127.0.0.1:8080

exit 0
