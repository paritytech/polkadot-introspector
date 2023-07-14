#!/bin/bash

ZOMBIENET_V=v1.3.63
ZOMBIENET_BIN=zombienet-linux-x64

case "$(uname -s)" in
    Linux*)     MACHINE=Linux;;
    Darwin*)    MACHINE=Mac;;
    *)          exit 127
esac

if [ $MACHINE != "Linux" ]; then
    exit 127
fi

zombienet_setup() {
  if [ ! -f $ZOMBIENET_BIN ]; then
    echo "fetching zombienet executable..."
    curl -LO https://github.com/paritytech/zombienet/releases/download/$ZOMBIENET_V/$ZOMBIENET_BIN
    chmod +x $ZOMBIENET_BIN
  fi
  ./$ZOMBIENET_BIN setup -y polkadot polkadot-parachain
}


print_help() {
  echo "This is a shell script to automate the execution of zombienet."
  echo ""
}

zombienet_run() {
  if [ ! -f $ZOMBIENET_BIN ]; then
    echo "zombienet binary not present, please run setup first"
  fi

  PATH=.:$PATH ./$ZOMBIENET_BIN -p native spawn $1
}

zombienet_shutdown() {
  ps -aux | grep zombienet | grep -v grep | awk '{print $2}' | xargs kill
}

SUBCOMMAND=$1
case $SUBCOMMAND in
  "" | "-h" | "--help")
    print_help
    ;;
  *)
    shift
    zombienet_${SUBCOMMAND} $@
    if [ $? = 127 ]; then
       echo "Error: This script can only run on Linux machines." >&2
       exit 1
    fi
  ;;
esac