# SupportBridge

This is a small utility to enable remote access to a TCP port (e.g. in order to serve SSH) to clients that are part of a protected network.
The scenario in which this is intendet wo work is that of a target device ("exposer") is in a protected network, not reachable from the internet and maybe even without any ability to reach the internet itself. Then the user, who has a machine that can reach the target device *and* the internet, can run a **relay** on her device to allow a tunnel connection through the internet to the target device.
The tunnel is established with the help of a publicly reachable **server**.
The user who wants to access the target device remotely can run the **client** on her machine, which connects to the target via the **server** and the **relay**.

Overview of all components:

* Server: publicly reachable websocket server
* Exposer: Runs on the target device and exposes a specified hostname/port to a websocket channel
* Client: Connects to the server. Listens on a TCP port and translates to websocket traffic, which is sent to the server.
* Relay: Tool that connects the exposer with the websocket server.


## Example

To run the full setup locally, open four terminal sessions and run the following:

    cargo run -- serve
    # [2024-08-20T09:34:58Z INFO  supportbridge::server] Listening on [::]:8081
    
    cargo run -- expose 100.93.114.74:22
    # [2024-08-20T09:41:18Z INFO  supportbridge::expose] Exposing 100.93.114.74:22 to [::]:8082
    
    cargo run -- relay localhost:8082 localhost:8081 demo
    # [2024-08-20T09:52:33Z INFO  supportbridge::bridge] Connected to exposed address: ws://localhost:8082/

Now, we do have a tunnel connection between the server and the exposer.
In order to connect to the exposed port on the exposer, we can either 
1. connect directly to the server (if the `-o` option was specified), where the port number is reported on the output of the server application or can be checked via the `list` subcommand.

    ssh -o StrictHostKeychecking=no -o UserKnownHostsFile=/dev/null -p 11000 user@localhost
    # ...should establish an SSH connection

2. or (if the `-o` option was not specified), connect via another instance of `supportbridge` that connects to the server via websockets and exposes a port locally:


    cargo run -- connect localhost:8081 demo
    # [2024-08-20T09:55:56Z INFO  supportbridge::client] Listening on [::]:8083

    ssh -o StrictHostKeychecking=no -o UserKnownHostsFile=/dev/null -p 8083 user@localhost
    # ...should establish an SSH connection

When the server is running behind a reverse proxy with TLS enabled, use the the following:
    cargo run -- relay localhost:8082 wss://supportbridge.example.com demo
    # [2024-08-20T09:52:33Z INFO  supportbridge::bridge] Connected to exposed address: ws://localhost:8082/

