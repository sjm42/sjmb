#!/bin/sh

set -x
set -e

PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PATH

tgt=/home/arska/arska/bin
rsync -var target/release/sjmb $tgt/

exit 0
# EOF
