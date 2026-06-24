# supportbridge

This is a small utility to enable remote access to a TCP port of a device that is part of a protected network.
The scenario in which this is intendet wo work is that of a target device ("exposer") is in a protected network, not reachable from the internet and maybe even without any ability to reach the internet itself. Then the user, who has a machine that can reach the target device *and* the internet, can run a **relay** on her device to allow a tunnel connection through the internet to the target device.
The tunnel is established with the help of a publicly reachable **server**.


## Features and Notes
* Due to the use of websockets for the tunnel, a relay can run fully in a web browser.
* Simple protocol and implementation in safe rust
* Authentication, authorization and encryption is left to inner layers. If the server is publicly reachable, it is recommended to put it behind a reverse proxy with proper SSL and let the reverse proxy handle authentication.
* Ideal for tunneling a SSH connection, which allows to expose additional ports on the target.



## Sub commands

* `supportbridge serve [<bind_addr_or_port>]`: run the (publicly reachable) websocket server that manages a list of connected exposers. **In production, make sure that only this port is exposed publicly. Ideally run it behind a reverse-proxy such as caddy or nginx.**.

* `supportbridge expose [<bind_addr_or_port>] [--server=<server_addr>] <target>`: Expose the `<target>` host:port via a supportbridge tunnel. If `<bind_addr_or_port>` is passed, a local port will be opened to which a supportbridge relay can connect and all traffic will be redirected to the target. If instead the `--server` option is specified, the exposed port is directly registered with the server so no relay is needed.

* `supportbridge relay <exposer> <server> <name>`: Registers the specified `<exposer>` with the `<server>` using the given `<name>` and forwards all traffic between them.


## Example

To run the full setup locally, open four terminal sessions and run the following:

    supportbridge serve
    # [2026-06-22T14:31:14Z INFO  supportbridge::server] Listening on [::]:8091
    
    supportbridge expose 100.93.114.74:22
    # [2026-06-22T14:39:39Z INFO  supportbridge::expose] Exposing to [::]:8092
    
    supportbridge relay localhost:8092 localhost:8091 demo
    # [2026-06-22T14:40:00Z INFO  supportbridge::util] URL scheme 'ws' authority: 'localhost:8091', path: '/register?name=demo'
    # [2026-06-22T14:40:00Z INFO  supportbridge::bridge] Connected to server: localhost:8091
    # [2026-06-22T14:40:00Z INFO  supportbridge::bridge] Connected to exposed address: localhost:8092


Now, we do have a tunnel connection between the server and the exposer.
Visit http://[::]:8091/list for a list of open tunnels.

In order to connect to the exposed port on the exposer, we can connect directly to the server, where the port number is reported on the output of the server application or can be checked via the list info page.

        ssh -o StrictHostKeychecking=no -o UserKnownHostsFile=/dev/null -p 11000 user@localhost
        # ...should establish an SSH connection

When the server is running behind a reverse proxy with TLS enabled, use the the following:

    supportbridge relay localhost:8082 wss://supportbridge.example.com demo
    # [2026-06-22T14:41:00Z INFO  supportbridge::bridge] Connected to exposed address: ws://localhost:8082/

