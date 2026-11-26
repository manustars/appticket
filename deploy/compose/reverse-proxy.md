# Behind a reverse proxy

The core speaks plain HTTP on the port `HD_HTTP_PORT` (8080), published on the host's loopback
address. HTTPS is the job of a reverse proxy in front of it: the one the host already has, such as
nginx, Traefik or Caddy, or for a host with none, Caddy started by the install with the
[profile `https`](#caddy-the-profile-https). What any proxy in front of the core must do, and how
the core uses `HD_PUBLIC_URL` and `HD_TRUSTED_PROXIES`, is in
[Behind a reverse proxy](../../docs/deploy.md#behind-a-reverse-proxy); this page has what is
particular to the Compose install.

## The proxy's address and HD_SUBNET

A proxy on the host passes requests on to `http://127.0.0.1:8080`, but the core does not see it
connect from `127.0.0.1`: Docker and Podman pass the connection on from the gateway of the
install's network, its first address. Caddy, and a proxy in a container that joins the network
`hd_default` (named after the project, `COMPOSE_PROJECT_NAME` when `.env` sets it), connect from
another address of that network. The network is `HD_SUBNET`
(`172.16.0.0/24` by default), and `HD_TRUSTED_PROXIES` is `HD_SUBNET` unless `.env` sets it: a
proxy on the host or Caddy needs nothing set.

Trusting the subnet trusts every container of the install, and every process of the host that
connects to the core's published port, since they all arrive from an address of it. Any of them
can write `X-Forwarded-For` and so get round the limit of failed sign-ins per address; the limit
per account still holds. Leave the core on the loopback address (`HD_HTTP_ADDRESS`). If its port
is opened to other hosts, set `HD_TRUSTED_PROXIES` in `.env` to the proxy's address alone, such
as the gateway, or empty to trust none.

The gateway's address was seen with Docker and with rootful Podman. With a rootless engine, which
passes connections on with a process of its own, the clients may all arrive from the gateway
whatever their address, so that every failed sign-in counts under one address: this has not been
tried, nor has Caddy on rootless Podman.

Change `HD_SUBNET` when it overlaps a network the host reaches:

- two installs on one host, each of which needs a subnet of its own;
- a LAN or VPN on `172.16.0.0/24`, such as one that the database, the mail server or the identity
  provider is on: the host would send their traffic to the install's network instead.

After changing it, or upgrading an install whose `compose.yaml` had no `HD_SUBNET`, stop the
install and start it again, so that every container is made anew on the new network:

```sh
docker compose down
docker compose up -d
```

`up -d` alone also makes the network anew, but Docker and Podman then put the containers back on
it without their service names, and the core no longer finds the database. The data stays on its
volume either way, and the trusted proxies follow `HD_SUBNET`.

## Examples

Each is for `helpdesk.example.com` and the port 8080: put in your host name, `HD_HTTP_PORT` and
certificate.

- **nginx**: [`examples/nginx.conf`](examples/nginx.conf), a file for `/etc/nginx/conf.d/`. The
  test of the install (`scripts/test-compose.sh`) runs this file.
- **Traefik**: [`examples/traefik.yaml`](examples/traefik.yaml), for Traefik's file provider, with
  the entry point `websecure` and the certificate resolver `letsencrypt` of Traefik's own
  configuration. A Traefik in a container reaches the core by joining the network `hd_default`,
  with `http://core:8080` as the server's URL. Not tested.
- **Caddy**: [`examples/Caddyfile`](examples/Caddyfile).

Traefik and Caddy add `X-Forwarded-For` and accept requests of any size by themselves; nginx is
told both.

## Caddy, the profile `https`

For a host with no reverse proxy, add `https` to `COMPOSE_PROFILES` in `.env`: Caddy then listens
on the ports 80 and 443 of every address of the host, sends plain HTTP to HTTPS, and passes
requests on to the core; without the profile it is not started. `HD_CADDY_HTTP_PORT` and
`HD_CADDY_HTTPS_PORT` choose other ports, for a host where Compose cannot open ports below 1024;
on another port than 443, plain HTTP is sent to the `https://` address without the port.
`HD_PUBLIC_URL` is the address Caddy serves, and [`Caddyfile`](Caddyfile) its configuration.
Caddy speaks HTTP/1.1 and HTTP/2, not HTTP/3.

`HD_HTTPS_CERTIFICATE` chooses the certificate:

- **`acme`** (the default): from Let's Encrypt, for the host name of `HD_PUBLIC_URL`. The name
  must lead to this host, and the ports 80 and 443 must be open to the Internet.
- **`files`**: one's own. Write the certificate, followed by its chain, to
  `secrets/tls_certificate` and its key to `secrets/tls_key`
  ([secrets/README.md](secrets/README.md)), and uncomment the lines `HD_SECRET_TLS_CERTIFICATE`
  and `HD_SECRET_TLS_KEY` of `.env`. After replacing them:
  `docker compose up -d --force-recreate caddy`.
- **`internal`**: from Caddy's own authority, for a LAN or a trial. The browsers trust it once
  they are given its root certificate, read with

  ```sh
  docker compose exec -T caddy cat /data/caddy/pki/authorities/local/root.crt > caddy-root.crt
  ```

  (`-T`, so that no terminal turns its line ends into CRLF) and imported as a trusted authority
  into the browsers or the system.

The certificates and Caddy's authority are kept on the volume `caddy-data`: `docker compose down
-v` removes them, and with them the root certificate the browsers were given.
