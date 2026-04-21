# Kubernetes

Deploy `rdmatop` as a DaemonSet to monitor RDMA traffic across all
GPU nodes in a Kubernetes cluster. The DaemonSet runs a privileged
container on each matching node, giving you per-node RDMA visibility
without SSH access.

## Build

Build the container image from the project root:

```bash
cd rdmatop
docker build -f examples/kubernetes/Dockerfile -t rdmatop:latest .
```

Push to your registry if needed:

```bash
docker tag rdmatop:latest <registry>/rdmatop:latest
docker push <registry>/rdmatop:latest
```

## Deploy

Before deploying, update the `image` field in `daemonset.yaml` to point to your registry (e.g. `<registry>/rdmatop:latest`) so Kubernetes can pull the image:

```bash
kubectl apply -f examples/kubernetes/daemonset.yaml
```

The DaemonSet targets GPU instance types (p5, p5e, p5en, p6e, p6-b200,
p6-b300) via node affinity. It uses `hostNetwork: true` so the container
sees the host's RDMA devices, and requests `NET_ADMIN` capability for
netlink access.

## Usage

Exec into a running pod to launch `rdmatop`:

```bash
# List pods
kubectl get pods -l app=rdmatop

# Attach to a specific node's pod
kubectl exec -it <pod-name> -- rdmatop
```

## Configuration

| Field | Description |
|-------|-------------|
| `hostNetwork: true` | Required — exposes host RDMA devices to the container |
| `NET_ADMIN` | Required — allows netlink queries for RDMA stats |
| `nodeAffinity` | Targets GPU instance types; edit `values` to match your cluster |
