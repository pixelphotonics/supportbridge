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

