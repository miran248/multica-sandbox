#!/bin/sh
# Pi stand-in: a pod in the scope namespace that can run psql, reach services by
# cluster DNS and touch the mounted task workdir.
trap 'exit 0' TERM INT
echo "harness-stub up scope=${SCOPE:-unknown} uid=$(id -u) host=$(hostname)"
while true; do sleep 3600; done
