# The Aether node of a demo container. run.sh sources it.

# forward <port>: the container address forwards to 127.0.0.1:<port>, so the
# host reaches a service that listens on 127.0.0.1 only.
forward() {
  socat "TCP-LISTEN:$1,bind=$(hostname -i | cut -d' ' -f1),fork,reuseaddr" "TCP:127.0.0.1:$1" &
}

# start_node <data dir> <admin port> <invite name> <log file>: the node with
# the connected adapter, in the background. Its admin page is forwarded to
# <admin port>.
start_node() {
  aether node serve --adapter connected --data-dir "$1" --port 4242 \
    --admin-port "$2" --hub-url "${AETHER_HUB_URL:-off}" --invite-name "$3" \
    >"$4" 2>&1 &
  forward "$2"
}
