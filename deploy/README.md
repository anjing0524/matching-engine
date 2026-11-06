# Deployment templates

These are **templates**, not a complete packaging story.

- `exchange.service` runs the implemented single-process runtime:
  `/opt/exchange/bin/exchange run`.
- `exchange-middleware` and `exchange-core` are implemented split-process
  binaries. They share command/public/private ring files created by middleware:
  core opens the command consumer and event producers, middleware opens the
  event consumers.
- `exchange-gateway` is a TCP relay: it accepts member frames and forwards them
  to the middleware gateway listener. Authentication and rate limiting still
  belong in a future gateway layer.
- `deploy/Dockerfile` builds a single-process runtime image; the compose example
  exposes 8000/tcp (gateway) and 9100/tcp (metrics).

## Install

```text
install -d /opt/exchange/bin /etc/exchange /run/exchange /var/lib/exchange
install -m 0644 deploy/exchange.env.example /etc/exchange/exchange.env
install -m 0644 deploy/exchange-*.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable exchange-middleware exchange-core exchange-gateway
```

Start order:

```text
exchange-middleware → exchange-core → exchange-gateway
```

## Notes

- `exchange-core` should run on a dedicated CPU set in production.
- Shared-memory files live under `/run/exchange` and must be recreated on boot.
- `ProtectSystem=strict` plus `ReadWritePaths` limits filesystem writes.
- Set `LimitMEMLOCK=infinity` if the deployment uses huge pages or mlocked rings.
- Prometheus should scrape the metrics listener on `127.0.0.1:9100/metrics`.
- No automatic core failover: use `Restart=on-failure` and operator replay.
