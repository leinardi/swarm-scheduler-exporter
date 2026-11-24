---
title: Home
template: home.html
hide: [navigation, toc]
home:
  tagline: Prometheus exporter for Docker Swarm focused on task state visibility, accurate desired replicas, and operability at scale.
  logo: assets/icon.svg
  actions:
    - { text: Get started, link: getting-started/, primary: true }
    - { text: GitHub, link: https://github.com/leinardi/swarm-scheduler-exporter }
---

<div class="rdt-install" markdown>

```bash
git clone https://github.com/leinardi/swarm-scheduler-exporter.git
cd swarm-scheduler-exporter/deployments/docker
docker stack deploy -c docker-compose.socket-proxy.yaml monitoring
```

</div>

## Features

<div class="grid cards" markdown>

- :lucide-target:{ .lg }

    **Accurate desired replicas**

    A global service counts only the nodes it can run on: node status, availability, placement
    constraints and platforms decide.

- :lucide-activity:{ .lg }

    **Live task state**

    The current task's state per slot, or per node for global services, with every known state
    emitted, zeros included.

- :lucide-refresh-cw:{ .lg }

    **Fresh without polling everything**

    Service and node events mark what changed for one reconciler, backed by a full resync every five
    minutes and after every reconnect.

- :lucide-shield-check:{ .lg }

    **Read-only by construction**

    The exporter only lists, inspects and follows events. A meta-test holds that boundary in the
    code, beyond what a `:ro` socket mount can.

</div>
