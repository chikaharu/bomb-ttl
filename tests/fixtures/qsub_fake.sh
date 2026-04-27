#!/usr/bin/env bash
# Test fixture: minimal `qsub` stand-in for bomb-ttl smoke tests.
# Mimics the real qsub's `[qsub] node …` line on stderr and then
# exits 0 immediately (no real sleep, no real rm).
set -u
addr_port=$(( RANDOM % 60000 + 1024 ))
echo "[qsub] node 127.0.0.1:${addr_port}  workdir /tmp/.tren-fake" 1>&2
echo "fake-stdout" 1>&1
exit 0
