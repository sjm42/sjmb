#!/bin/sh

set -x
set -e

PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
export PATH

if [ $(uname -n) = "sorvi" ]
then
  tgt=/home/arska/arska/bin
else
  tgt=$HOME/sjmb/bin
fi

rsync -var target/release/sjmb $tgt/

exit 0
# EOF
