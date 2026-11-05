# Aether two-VM direct-path runbook

This runbook uses two OrbStack Debian VMs. It proves a direct Iroh exchange
between separate VM processes. It does not prove a path between physical
computers.

Create the VMs:

```sh
orbctl create --cpus 1 --memory 2G --disk 10G debian:12 aether-node-one
orbctl create --cpus 1 --memory 2G --disk 10G debian:12 aether-node-two
```

Build a Linux binary and copy it from the test image:

```sh
docker build -f Dockerfile.e2e -t aether-e2e:local .
docker create --name aether-e2e-export aether-e2e:local
docker cp aether-e2e-export:/usr/local/bin/aether /private/tmp/aether-vm
docker rm aether-e2e-export
```

Get the first VM address:

```sh
orbctl list
```

Start the server in the first VM:

```sh
orbctl run -m aether-node-one sh -c 'nohup /mnt/mac/private/tmp/aether-vm node serve --data-dir /tmp/aether-server --port 4242 >/tmp/aether-server.log 2>&1 &'
orbctl run -m aether-node-one cat /tmp/aether-server.log
```

Copy the `endpoint_id` from the log. Run the client in the second VM. Replace
`ENDPOINT_ID` and `FIRST_VM_IP`:

```sh
orbctl run -m aether-node-two /mnt/mac/private/tmp/aether-vm node request --data-dir /tmp/aether-client --endpoint-id ENDPOINT_ID --address FIRST_VM_IP:4242 --scope direct --text review
```

The result must contain an `event` frame and a `result` frame with `ok`.
