# Deployment templates

These are **templates**, not a complete packaging story. The repository does not
yet provide `exchange-core`, `exchange-middleware`, or `exchange-gateway`
binaries; the units assume those entry points are installed under
`/opt/exchange/bin` and configured via `/etc/exchange/*.env`.

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
