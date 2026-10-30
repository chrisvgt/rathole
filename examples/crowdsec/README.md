# CrowdSec for the rathole server port

The server port (`server.bind_addr`) has to be reachable from the internet. Every connection that does not finish the handshake is logged with the IP of the peer:

```
ERROR connection{addr=203.0.113.5:51234}: rathole::server: Handshake from 203.0.113.5 failed: Failed to do transport handshake: Failed to do noise handshake: decrypt error
ERROR connection{addr=203.0.113.5:51234}: rathole::server: Handshake from 203.0.113.5 failed: Transport handshake timeout: deadline has elapsed
ERROR connection{addr=203.0.113.5:51234}: rathole::server: Handshake from 203.0.113.5 failed: Service app failed the authentication
```

These files let [CrowdSec](https://www.crowdsec.net/) ban IPs that fail more than 5 times in a short time. They assume rathole runs as `rathole.service` and logs to the journal. With the units from [examples/systemd](../systemd) the name is e.g. `ratholes@app1.service`, change it in `acquis.yaml`.

| File | Copy to |
|---|---|
| `acquis.yaml` | `/etc/crowdsec/acquis.d/rathole.yaml` |
| `rathole-logs.yaml` | `/etc/crowdsec/parsers/s01-parse/rathole-logs.yaml` |
| `rathole-bf.yaml` | `/etc/crowdsec/scenarios/rathole-bf.yaml` |

```sh
sudo cscli parsers install crowdsecurity/syslog-logs crowdsecurity/dateparse-enrich
sudo systemctl reload crowdsec

# Check that a log line is parsed
journalctl -u rathole -g 'Handshake from' -n 1 -o short > /tmp/rathole.log
sudo cscli explain --file /tmp/rathole.log --type syslog
```

The bans are enforced by a bouncer, e.g. `crowdsec-firewall-bouncer-nftables`. It blocks the IP for the whole machine.

Your own client fails too if its key or token is wrong, and would then be banned. If its IP is fixed, add it to a [whitelist](https://docs.crowdsec.net/docs/whitelist/intro). With a fixed IP you can also open the server port only for that IP in the firewall, which is stronger than any ban.
