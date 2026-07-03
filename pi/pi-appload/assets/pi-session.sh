#!/bin/sh
# Runs INSIDE yaft on the tablet. yaft itself is started by AppLoad with the
# qtfb shim preloaded; pi and its children must not inherit that shim.
unset LD_PRELOAD QTFB_SHIM_MODEL QTFB_SHIM_MODE QTFB_KEY

export HOME=/home/root
export PATH=/home/root/bin:/home/root/opt/node/bin:/usr/bin:/bin:/usr/sbin:/sbin
export TERM=yaft-256color
export TERMINFO=/home/root/.terminfo
cd /home/root

if [ ! -x /home/root/bin/pi ]; then
    echo "pi is not installed on the tablet."
    echo "From your Mac: ./pi-harness/install.sh root@<tablet-ip>"
    echo
    echo "[press enter to close]"
    read _
    exit 1
fi

/home/root/bin/pi
rc=$?
echo
echo "pi exited (status $rc). [press enter to close]"
read _
