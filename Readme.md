# supportbridge

This is a small utility to enable remote access to a TCP port of a device that is part of a protected network.
The scenario in which this is intendet wo work is that of a target device ("exposer") is in a protected network, not reachable from the internet and maybe even without any ability to reach the internet itself. Then the user, who has a machine that can reach the target device *and* the internet, can run a **relay** on her device to allow a tunnel connection through the internet to the target device.
The tunnel is established with the help of a publicly reachable **server**.


## Features and Notes
* Due to the use of websockets for the tunnel, a relay can run fully in a web browser.
* Simple protocol and implementation in 500 lines of safe rust
* Only allows a single connection to an exposer at a time. Opening a new connection will close the old one.
* Authentication authorization and encryption is left to inner layers. If the server is publicly reachable, it is recommended to put it behind a reverse proxy with proper SSL and let the reverse proxy handle authentication.
* Ideal for tunneling a SSH connection, which allows to expose additional ports on the target.



## Sub commands

* `supportbridge serve [<bind_addr_or_port>]`: publicly reachable websocket server that manages a list of connected exposers.
  
  If run with `--open-ports`, it will expose one TCP port on the server that maps directly to each connected exposer. In this case, a connection to the exposer can be made by directly connecting to the opened port on the server.
  If omitted, a connection to the exposer via the server can be made by running `supportbridge open`, which will locally open a TCP port which maps to an exposer port.

* `supportbridge expose [<bind_addr_or_port>] [--server=<server_addr>] <target>`: Expose the `<target>` host:port via a supportbridge tunnel. If `<bind_addr_or_port>` is passed, a local port will be opened to which a supportbridge relay can connect and all traffic will be redirected to the target. If instead the `--server` option is specified, the exposed port is directly registered with the server so no relay is needed.

* `supportbridge relay <exposer> <server> <name>`: Registers the specified `<exposer>` with the `<server>` using the given `<name>` and forwards all traffic between them.


## Example

To run the full setup locally, open four terminal sessions and run the following:

    supportbridge serve --open-ports
    # [2024-08-20T09:34:58Z INFO  supportbridge::server] Listening on [::]:8081
    
    supportbridge expose 100.93.114.74:22
    # [2024-08-20T09:41:18Z INFO  supportbridge::expose] Exposing 100.93.114.74:22 to [::]:8082
    
    supportbridge relay localhost:8082 localhost:8081 demo
    # [2024-08-20T09:52:33Z INFO  supportbridge::bridge] Connected to exposed address: ws://localhost:8082/

    supportbridge list localhost:8081
    # | Name  | Peer         | Open since            | Server port  | Occupied   |
    # | ====  | ====         | ==========            | ===========  | ========   |
    # | demo  | [::1]:35428  | 2024-09-03T17:48:40Z  | -            | -          |

Now, we do have a tunnel connection between the server and the exposer.
In order to connect to the exposed port on the exposer, we can either 
1. connect directly to the server (if the `-o` option was specified), where the port number is reported on the output of the server application or can be checked via the `list` subcommand.

        ssh -o StrictHostKeychecking=no -o UserKnownHostsFile=/dev/null -p 11000 user@localhost
        # ...should establish an SSH connection

2. or (if the `-o` option was not specified), connect via another instance of `supportbridge` that connects to the server via websockets and exposes a port locally:

        supportbridge open localhost:8081 demo
        # [2024-08-20T09:55:56Z INFO  supportbridge::client] Listening on [::]:8083

        ssh -o StrictHostKeychecking=no -o UserKnownHostsFile=/dev/null -p 8083 user@localhost
        # ...should establish an SSH connection

When the server is running behind a reverse proxy with TLS enabled, use the the following:

    supportbridge relay localhost:8082 wss://supportbridge.example.com demo
    # [2024-08-20T09:52:33Z INFO  supportbridge::bridge] Connected to exposed address: ws://localhost:8082/

