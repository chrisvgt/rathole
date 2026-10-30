## Systemd Unit Examples

Unit files to run `rathole` as a service on Linux. Each one is a template, the part after `@` names the config file, so `ratholes@app1` runs `rathole --server /etc/rathole/app1.toml`.

- `ratholes@.service`: `rathole --server`
- `ratholec@.service`: `rathole --client`
- `rathole@.service`: `rathole`, which picks the mode from the config

The services run as the unprivileged system user `rathole` and are restricted with the sandboxing options of systemd. The server may bind ports below 1024, the client may not.

### Setup

Assuming `rathole` is installed in `/usr/bin/rathole`, these steps run `rathole --server` with the config `/etc/rathole/app1.toml`.

1. Create the user and the config directory. Only root and the `rathole` group can read the configs, they contain the tokens and keys.

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin rathole
sudo install -d -m 750 -o root -g rathole /etc/rathole
```

2. Create the config and make it readable for the service.

```bash
sudo touch /etc/rathole/app1.toml
sudo chown root:rathole /etc/rathole/app1.toml
sudo chmod 640 /etc/rathole/app1.toml
# Now edit /etc/rathole/app1.toml
```

3. Install the unit, then enable and start the service.

```bash
sudo cp ratholes@.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable ratholes@app1 --now
```

### Run multiple services

Add another config, say `/etc/rathole/app2.toml`, and run `sudo systemctl enable ratholes@app2 --now`. The same applies to `ratholec@.service` and `rathole@.service`.

### TLS files

With TLS, the files from `pkcs12` and `trusted_root` must be readable by the `rathole` group as well, e.g. in `/etc/rathole` with the same owner and mode as the config.
